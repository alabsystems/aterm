// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's default writer (T0): every session's derived title and description, the pure ladder
//! and description table, a slot's subject moving to its agent and back, its own bucket, a refused
//! rung and a refused description, the agent's words taken at a trigger, the claims a seamless
//! update carried kept until their inputs are read again, an excerpt re-read that moved nothing,
//! the lane's ring with the lane unpublished, and an agent's first prompt: picked, kept, moved by
//! its prompts alone, and never picked again after an update once kept.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aterm_agent::harness::excerpt::{Excerpts, Prompt, prompt_key};
use aterm_slog::{Actor, Kind, Node, Rank, Src, Stamp, Tier, TitleSrc};

use super::{
    DESCRIPTION, Ended, LANE, Line, Lines, Read, Rung, Said, Seen, TITLE, Taken, Trigger, UNNAMED,
    Want, derive, line_of, notify, pass, ring, words_read_for, write,
};
use crate::session_log::{BlockEnd, BlockFact, BlockRead, Clock, DescRefused, LogAdopt, LogCarry};
use crate::session_store::{SessionHandle, SessionHandoff, Store, new_store, test_handle};
use crate::session_timeline::{MetaEdit, MetaField, SessionTimeline, write_session_meta};
use crate::survey::{ClaudeExcerpts, WordsRead};

const WALL0: u64 = 1_760_000_000_000;

/// The task tracker package ST-02's spawns give: a blank first line, the line, then more.
const TASK_TEXT: &[u8] = b"\n  Fix the flaky title test\nthen run verify\n";

/// [`TASK_TEXT`]'s first line that draws something.
const TASK_LINE: &str = "Fix the flaky title test";

/// What the agent of the exit test said at the end of its turn.
const CLOSING: &str = "All green; landing the branch.";

/// The folder the shells of these tests sit in (outside any home, so shown as it is).
const FOLDER: &str = "/srv/demo-aterm";

/// A hand-driven clock: `ms` into the test, on both of a session log's clocks.
#[derive(Clone)]
struct Hand(Arc<AtomicU64>);

impl Hand {
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }

    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

/// Put `timeline`'s session log on a hand clock at 0.
fn hand_clock(timeline: &Mutex<SessionTimeline>) -> Hand {
    hand_clock_from(timeline, 0)
}

/// Put `timeline`'s session log on a hand clock at 0, its wall clock `wall` ms into the test: a
/// new process's, whose monotonic clock starts again.
fn hand_clock_from(timeline: &Mutex<SessionTimeline>, wall: u64) -> Hand {
    let ms = Arc::new(AtomicU64::new(0));
    let read = Arc::clone(&ms);
    timeline
        .lock()
        .unwrap()
        .slog_mut()
        .set_clock(Clock::reading(move || {
            let ms = read.load(Ordering::SeqCst);
            Stamp::new(WALL0 + wall + ms, ms)
        }));
    Hand(ms)
}

/// A session registered in `store` (its log opened by the register), on a hand clock, its root
/// shell known.
fn registered(store: &Store, local: u64) -> (SessionHandle, Hand) {
    let h = test_handle(local);
    let hand = hand_clock(&h.ctx.timeline);
    store.write().unwrap().register(h.clone());
    h.ctx.timeline.lock().unwrap().note_root_group(100);
    (h, hand)
}

/// A shell command block `blk` that started `start` ms into the test, as the status sweep reads
/// it: its program `detail` confirmed, or none.
fn fact(blk: u64, start: u64, end: BlockEnd, detail: Option<&str>) -> BlockFact {
    BlockFact {
        id: blk,
        started_ms: Some(WALL0 + start),
        end,
        detail: detail.map(str::to_string),
        detail_final: true,
        shape: crate::session_status::LineShape::ONE,
    }
}

/// Give `h`'s shell `read` at `ms`.
fn blocks(h: &SessionHandle, hand: &Hand, ms: u64, read: &BlockRead) {
    hand.set(ms);
    let mut g = h.ctx.timeline.lock().unwrap();
    let now = g.slog().now();
    g.slog_mut().shell_blocks(read, None, now);
}

/// A block `blk` that ran `detail` from `start` to `end` and exited with `code`.
fn finished(blk: u64, code: i32, start: u64, end: u64, detail: &str) -> BlockRead {
    let done = BlockEnd::Done(crate::session_status::CompletedBlock {
        id: blk,
        exit_code: Some(code),
        started_ms: Some(WALL0 + start),
        ended_ms: Some(WALL0 + end),
    });
    BlockRead {
        facts: vec![fact(blk, start, done, Some(detail))],
        ..BlockRead::default()
    }
}

/// A block `blk` running since `start` whose program is not yet confirmed.
fn running_unnamed(blk: u64, start: u64) -> BlockRead {
    BlockRead {
        facts: vec![fact(blk, start, BlockEnd::Running, None)],
        ..BlockRead::default()
    }
}

/// The drift flush read `cwd` off `h`'s terminal.
fn in_folder(h: &SessionHandle, cwd: &str) {
    let _ = h.ctx.timeline.lock().unwrap().record_cwd_change(Some(cwd));
}

/// Claude Code in front of `h`'s shell, its run open: the run.
fn with_claude(h: &SessionHandle) -> Node {
    let mut g = h.ctx.timeline.lock().unwrap();
    assert!(g.note_foreground_group(300));
    assert!(g.set_program(300, Some("claude".into())));
    let pgid = g.agent().program_pgid;
    let _ = g.sweep_slog(&BlockRead::default(), false, 100, pgid);
    g.slog()
        .log()
        .state()
        .desc
        .open_agent()
        .expect("PRECONDITION: the agent's run is open")
}

/// The shell takes the front back from the agent, whose run ends.
fn agent_ends(h: &SessionHandle, run: Node) {
    let mut g = h.ctx.timeline.lock().unwrap();
    assert!(g.note_foreground_group(100));
    assert!(g.set_program(100, Some("zsh".into())));
    let pgid = g.agent().program_pgid;
    let _ = g.sweep_slog(&BlockRead::default(), false, 100, pgid);
    assert!(
        !g.slog().log().is_open(run),
        "PRECONDITION: the agent's run ended"
    );
}

/// The excerpts of Claude Code in front of `h`: what it said as it went and at the end.
fn said(h: &SessionHandle, said: &str, said_end: &str) {
    let mut g = h.ctx.timeline.lock().unwrap();
    let pgid = g.agent().program_pgid;
    g.set_claude_excerpts(
        pgid,
        Some(ClaudeExcerpts {
            excerpts: Excerpts {
                prompts: Vec::new(),
                said: Some(said.into()),
                said_end: Some(said_end.into()),
                said_at_ms: None,
                doing: Some("Bash".into()),
            },
            task: None,
            transcript_len: None,
        }),
    );
}

/// The excerpts of Claude Code in front of `h` holding the typed prompts `prompts`, newest first,
/// and nothing it said: whether what T0 reads of them moved (the setter's answer, which rings the
/// lane).
fn prompted(h: &SessionHandle, prompts: &[&str]) -> bool {
    let typed: Vec<(&str, bool)> = prompts.iter().map(|text| (*text, false)).collect();
    prompted_kinds(h, &typed)
}

/// [`prompted`] with each prompt's kind: `(text, true)` a bash-mode command (`! cmd`).
fn prompted_kinds(h: &SessionHandle, prompts: &[(&str, bool)]) -> bool {
    let mut g = h.ctx.timeline.lock().unwrap();
    let pgid = g.agent().program_pgid;
    g.set_claude_excerpts(
        pgid,
        Some(ClaudeExcerpts {
            excerpts: Excerpts {
                prompts: prompts
                    .iter()
                    .map(|(text, shell)| Prompt {
                        text: (*text).to_owned(),
                        key: prompt_key(text),
                        shell: *shell,
                    })
                    .collect(),
                ..Excerpts::default()
            },
            task: None,
            transcript_len: None,
        }),
    )
}

/// `h`'s task, as a spawn gives it: working, its first line [`TASK_LINE`].
fn tasked(h: &SessionHandle) {
    use crate::task_record::{FROM_CTL, TaskMark, TaskState, mark_session_task, task_line};
    let mut mark = TaskMark::new(TaskState::Working);
    mark.from = Some(FROM_CTL.to_owned());
    mark_session_task(&h.ctx, h.local_id, &mark).expect("unfenced");
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .set_task_line(task_line(TASK_TEXT));
}

/// What a reader is shown of `slot` of `h`.
#[derive(Debug, PartialEq, Eq)]
struct ShownLine {
    text: String,
    scope: Node,
    rank: Rank,
    by: Actor,
    as_name: Option<String>,
}

fn shown(h: &SessionHandle, slot: &str) -> Option<ShownLine> {
    let g = h.ctx.timeline.lock().unwrap();
    let now = g.slog().now().wall_ms;
    aterm_slog::shown(g.slog().log().state(), slot, now).map(|s| ShownLine {
        text: s.claim.value.clone(),
        scope: s.scope,
        rank: s.rank,
        by: s.claim.by,
        as_name: s.claim.as_name.clone(),
    })
}

/// T0's own claim of `slot` on `scope`, standing or not: its text and `as=`.
fn derived(h: &SessionHandle, scope: Node, slot: &str) -> Option<(String, Option<String>)> {
    let g = h.ctx.timeline.lock().unwrap();
    g.slog()
        .log()
        .state()
        .desc
        .claim_of(scope, slot, Actor::Aterm, Src::Aterm, None)
        .map(|c| (c.value.clone(), c.as_name.clone()))
}

/// The newest seq of `h`'s log.
fn last_seq(h: &SessionHandle) -> u64 {
    h.ctx.timeline.lock().unwrap().slog().log().last_seq()
}

/// Every record of `h`'s log, encoded at `tier`.
fn records(h: &SessionHandle, tier: Tier) -> Vec<String> {
    let g = h.ctx.timeline.lock().unwrap();
    g.slog()
        .log()
        .window()
        .map(|r| aterm_slog::encode(r, tier).expect("a logged record fits"))
        .collect()
}

/// `h`'s `desc` records, at the host tier.
fn desc_records(h: &SessionHandle) -> Vec<String> {
    let g = h.ctx.timeline.lock().unwrap();
    g.slog()
        .log()
        .window()
        .filter(|r| r.k == Kind::Desc)
        .map(|r| aterm_slog::encode(r, Tier::Host).expect("a logged record fits"))
        .collect()
}

/// EVERY SESSION HAS A DERIVED TITLE AND DESCRIPTION (ST-29's exit test): after one pass, a
/// shell with a folder whose `make` failed, an agent with a task line and closing words, and a
/// session with nothing observed each have a `title` and a `description` a reader is shown,
/// written by aterm at the derived rank. The shell at its prompt is titled by its folder and
/// described as idle with its last command's exit; the agent's title is its task's line on the
/// session (`as=task`) and its description, on the agent's run, its closing words; the bare
/// session reads `shell` and `idle`.
///
/// CONTROLS: a second pass with nothing new appends no record; a person's title stands over
/// T0's, and a pass after it writes nothing over it; a command line never enters a title: a
/// shell whose terminal title is a command line, running a command whose program is not
/// confirmed, is titled by its folder and no record holds the line.
#[test]
fn every_session_has_a_derived_title_and_description() {
    let store = new_store();
    let (shell, shell_hand) = registered(&store, 1);
    let (agent, _agent_hand) = registered(&store, 2);
    let (bare, _bare_hand) = registered(&store, 3);
    in_folder(&shell, FOLDER);
    blocks(&shell, &shell_hand, 100, &finished(0, 2, 10, 40, "make"));
    tasked(&agent);
    let run = with_claude(&agent);
    said(&agent, "Running the tests now.", CLOSING);
    assert!(
        agent
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()
            .state()
            .turn()
            .is_none(),
        "PRECONDITION: the agent's turn is closed, so its words are its closing ones"
    );

    assert_eq!(pass(&store), None, "nothing is owed after the pass");
    for h in [&shell, &agent, &bare] {
        for slot in [TITLE, DESCRIPTION] {
            let line = shown(h, slot).unwrap_or_else(|| panic!("{slot} of {} shown", h.local_id));
            assert_eq!(
                (line.rank, line.by),
                (Rank::Derived, Actor::Aterm),
                "{slot} of {}: {line:?}",
                h.local_id
            );
        }
    }
    assert_eq!(
        shown(&shell, TITLE).map(|l| (l.text, l.scope, l.as_name)),
        Some((FOLDER.to_owned(), Node::SESSION, Some("folder".to_owned()))),
        "a shell at its prompt is titled by its folder"
    );
    assert_eq!(
        shown(&shell, DESCRIPTION).map(|l| (l.text, l.scope)),
        Some(("idle · make exit 2".to_owned(), Node::SESSION))
    );
    assert_eq!(
        shown(&agent, TITLE).map(|l| (l.text, l.scope, l.as_name)),
        Some((TASK_LINE.to_owned(), Node::SESSION, Some("task".to_owned()))),
        "the task's line titles the session"
    );
    assert_eq!(
        shown(&agent, DESCRIPTION).map(|l| (l.text, l.scope)),
        Some((CLOSING.to_owned(), run)),
        "the agent's closing words describe its run"
    );
    assert_eq!(
        shown(&bare, TITLE).map(|l| (l.text, l.as_name)),
        Some((UNNAMED.to_owned(), None))
    );
    assert_eq!(
        shown(&bare, DESCRIPTION).map(|l| l.text),
        Some("idle".to_owned())
    );

    let seqs = [last_seq(&shell), last_seq(&agent), last_seq(&bare)];
    assert_eq!(pass(&store), None);
    assert_eq!(
        [last_seq(&shell), last_seq(&agent), last_seq(&bare)],
        seqs,
        "CONTROL: a second pass with nothing new appends no record"
    );

    write_session_meta(
        &shell.ctx,
        MetaField::Title,
        MetaEdit::Set("Release notes"),
        Actor::Human,
    )
    .expect("a person's rename");
    let renamed = last_seq(&shell);
    assert_eq!(pass(&store), None);
    assert_eq!(
        last_seq(&shell),
        renamed,
        "CONTROL: T0 writes nothing over a person's title"
    );
    assert_eq!(
        shown(&shell, TITLE).map(|l| (l.text, l.rank)),
        Some(("Release notes".to_owned(), Rank::Person)),
        "CONTROL: the person's title stands over T0's"
    );
    assert_eq!(
        derived(&shell, Node::SESSION, TITLE),
        Some((FOLDER.to_owned(), Some("folder".to_owned()))),
        "and T0's stays listed beneath it"
    );

    let (typed, typed_hand) = registered(&store, 4);
    in_folder(&typed, FOLDER);
    crate::term_lock(&typed.term).process(b"\x1b]2;make -j8 deploy-QZP29\x07");
    blocks(&typed, &typed_hand, 100, &running_unnamed(0, 50));
    assert_eq!(pass(&store), None);
    assert_eq!(
        shown(&typed, TITLE).map(|l| (l.text, l.as_name)),
        Some((FOLDER.to_owned(), Some("folder".to_owned()))),
        "CONTROL: a run whose word is unconfirmed titles by the folder"
    );
    assert_eq!(
        shown(&typed, DESCRIPTION).map(|l| l.text),
        Some("running".to_owned())
    );
    let content = records(&typed, Tier::Content);
    assert!(
        content.iter().all(|r| !r.contains("QZP29")),
        "CONTROL: no record holds the command line: {content:?}"
    );
}

/// THE LADDER AND THE DESCRIPTION, as the pure [`derive`] gives them: the title's rungs in order
/// (the task's line on the session; the program and folder, the folder, the program on the
/// subject; `shell` last), and the description's: for an agent its need over its newer topic
/// post over its words over whether it is working, the post only where newer than the words;
/// for a shell what runs or `idle`, then its last command's word and how it ended.
#[test]
fn the_ladder_and_the_description_are_pure() {
    let run = Node::parse("r4").expect("a run");
    let titles = |read: &Read| -> Vec<(Node, String, Option<Rung>)> {
        derive(read)
            .title
            .into_iter()
            .map(|l| (l.on, l.text, l.rung))
            .collect()
    };
    let described = |read: &Read| -> Vec<String> {
        derive(read)
            .description
            .into_iter()
            .map(|l| l.text)
            .collect()
    };
    let full = Read {
        program: Some("vim".into()),
        running: true,
        task_line: Some(TASK_LINE.into()),
        folder: Some("~/aterm".into()),
        ..Read::default()
    };
    assert_eq!(
        titles(&full),
        [
            (Node::SESSION, TASK_LINE.to_owned(), Some(Rung::Task)),
            (
                Node::SESSION,
                "vim · ~/aterm".to_owned(),
                Some(Rung::Program)
            ),
            (Node::SESSION, "~/aterm".to_owned(), Some(Rung::Folder)),
            (Node::SESSION, "vim".to_owned(), Some(Rung::Program)),
            (Node::SESSION, UNNAMED.to_owned(), None),
        ]
    );
    assert_eq!(described(&full), ["running vim"]);
    let agent = Read {
        agent: Some(run),
        program: Some("claude".into()),
        running: true,
        ..full.clone()
    };
    assert_eq!(
        titles(&agent).iter().map(|t| t.0).collect::<Vec<_>>(),
        [Node::SESSION, run, run, run, run],
        "the task's line is the session's; every other rung the agent's"
    );
    assert_eq!(
        derive(&agent)
            .description
            .iter()
            .map(|l| l.on)
            .collect::<Vec<_>>(),
        [run],
        "the description is the agent's"
    );
    let prompted = Read {
        prompt: Some("wire the survey".into()),
        ..agent.clone()
    };
    assert_eq!(
        titles(&prompted)
            .into_iter()
            .map(|(on, _, rung)| (on, rung))
            .collect::<Vec<_>>(),
        [
            (Node::SESSION, Some(Rung::Task)),
            (run, Some(Rung::Prompt)),
            (run, Some(Rung::Program)),
            (run, Some(Rung::Folder)),
            (run, Some(Rung::Program)),
            (run, None),
        ],
        "an agent's first prompt, on its run, right after the task's line"
    );
    assert_eq!(titles(&prompted)[1].1, "wire the survey");
    assert_eq!(
        titles(&Read {
            agent: None,
            ..prompted.clone()
        })
        .iter()
        .filter(|t| t.2 == Some(Rung::Prompt))
        .count(),
        0,
        "no agent run, no prompt rung"
    );
    assert_eq!(
        [Rung::Task, Rung::Prompt, Rung::Program, Rung::Folder].map(Rung::word),
        ["task", "prompt", "program", "folder"],
        "a rung's `as=` is its `title_src` word"
    );
    assert_eq!(
        titles(&Read::default()),
        [(Node::SESSION, UNNAMED.to_owned(), None)],
        "nothing observed: `shell`"
    );
    assert_eq!(described(&Read::default()), ["idle"]);
    let programless = Read {
        program: Some("htop".into()),
        running: true,
        ..Read::default()
    };
    assert_eq!(
        titles(&programless)[0],
        (Node::SESSION, "htop".to_owned(), Some(Rung::Program)),
        "no folder: the program alone"
    );

    let ended = |word: &str, ended: Ended| Read {
        last_cmd: Some((word.into(), ended)),
        ..Read::default()
    };
    assert_eq!(
        described(&ended("make", Ended::Exit(0))),
        ["idle · make exit 0"]
    );
    assert_eq!(
        described(&ended("make", Ended::Exit(2))),
        ["idle · make exit 2"]
    );
    assert_eq!(
        described(&ended("sleep", Ended::Signal)),
        ["idle · sleep signal"]
    );
    assert_eq!(described(&ended("make", Ended::Unknown)), ["idle · make"]);
    assert_eq!(
        described(&Read {
            running: true,
            ..ended("make", Ended::Exit(1))
        }),
        ["running · make exit 1"],
        "a run whose word is unconfirmed is only `running`"
    );

    let speaking = Read {
        agent: Some(run),
        program: Some("claude".into()),
        need: Some("approval · bash".into()),
        words: Some(("Running the tests.".into(), 2_000)),
        topic: Some(("crate 3 of 9".into(), 3_000)),
        ..Read::default()
    };
    assert_eq!(
        described(&speaking),
        [
            "approval · bash",
            "crate 3 of 9",
            "Running the tests.",
            "claude idle"
        ],
        "a need over a newer post over the words over the agent's state"
    );
    let no_need = Read {
        need: None,
        ..speaking.clone()
    };
    assert_eq!(
        described(&no_need)[0],
        "crate 3 of 9",
        "a post newer than the words"
    );
    let older_post = Read {
        topic: Some(("crate 3 of 9".into(), 1_000)),
        ..no_need.clone()
    };
    assert_eq!(
        described(&older_post),
        ["Running the tests.", "claude idle"],
        "a post older than the words is not read"
    );
    let tie = Read {
        topic: Some(("crate 3 of 9".into(), 2_000)),
        ..no_need.clone()
    };
    assert_eq!(
        described(&tie)[0],
        "Running the tests.",
        "a tie is the words'"
    );
    let silent = Read {
        words: None,
        topic: None,
        turn_open: true,
        ..no_need
    };
    assert_eq!(described(&silent), ["claude working"]);

    assert_eq!(
        line_of("two\nlines\u{200b} here\tnow", 256).as_deref(),
        Some("two lines here now"),
        "one line, no hidden character"
    );
    let long = "word ".repeat(80);
    let cut = line_of(&long, 256).expect("a line");
    assert!(
        aterm_codec::pct::encoded_len(&cut) <= 256 && cut.ends_with("word"),
        "cut at a word within 256 encoded bytes: {cut:?}"
    );
    assert_eq!(line_of("\u{200b}", 256), None);
}

/// A SLOT'S SUBJECT MOVES TO THE AGENT AND BACK: T0's lines of a shell are claimed on the session;
/// once an agent runs, its program and folder title and its description are claimed on the
/// agent's run and T0's session claims are withdrawn first (a tie would show the session's); once
/// the agent ends, its claims lapse with it and the next pass claims the session's again.
///
/// CONTROL: the withdrawals are T0's own records, one a slot, with no value.
#[test]
fn a_slots_subject_moves_to_the_agent_and_back_with_a_withdrawal() {
    let store = new_store();
    let (h, _hand) = registered(&store, 11);
    in_folder(&h, FOLDER);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&h, Node::SESSION, TITLE).map(|t| t.0),
        Some(FOLDER.to_owned())
    );
    assert_eq!(
        derived(&h, Node::SESSION, DESCRIPTION).map(|t| t.0),
        Some("idle".to_owned())
    );

    let run = with_claude(&h);
    let before = desc_records(&h).len();
    assert_eq!(pass(&store), None);
    let title = format!("claude · {FOLDER}");
    assert_eq!(
        shown(&h, TITLE).map(|l| (l.text, l.scope, l.as_name)),
        Some((title.clone(), run, Some("program".to_owned())))
    );
    assert_eq!(
        shown(&h, DESCRIPTION).map(|l| (l.text, l.scope)),
        Some(("claude idle".to_owned(), run))
    );
    assert_eq!(
        derived(&h, Node::SESSION, TITLE),
        None,
        "withdrawn from the session"
    );
    assert_eq!(derived(&h, Node::SESSION, DESCRIPTION), None);
    let moved = desc_records(&h)[before..].to_vec();
    let withdrawals: Vec<&String> = moved
        .iter()
        .filter(|r| r.contains(" mark - desc ") && !r.contains(" value="))
        .collect();
    assert_eq!(
        withdrawals.len(),
        2,
        "CONTROL: one withdrawal a slot, on the session: {moved:?}"
    );
    assert!(
        withdrawals.iter().all(|r| r.contains(" by=aterm ")),
        "{withdrawals:?}"
    );

    agent_ends(&h, run);
    assert_eq!(
        derived(&h, run, TITLE),
        None,
        "the agent's claims lapsed with it"
    );
    assert_eq!(pass(&store), None);
    assert_eq!(
        shown(&h, TITLE).map(|l| (l.text, l.scope)),
        Some((FOLDER.to_owned(), Node::SESSION))
    );
    assert_eq!(
        shown(&h, DESCRIPTION).map(|l| (l.text, l.scope)),
        Some(("idle".to_owned(), Node::SESSION))
    );
}

/// A SHORT BUCKET LEAVES THE NEWEST LINE OWED, AND THE NEXT PASS WRITES IT: T0's own bucket
/// pays for twenty passes that write at once; the twenty-first writes nothing and says when the
/// bucket next holds a token; the folder moving again meanwhile changes nothing written; once a
/// second has passed, the next pass writes the newest folder, not the one first owed.
#[test]
fn a_short_bucket_leaves_the_newest_line_owed_and_the_next_pass_writes_it() {
    let store = new_store();
    let (h, hand) = registered(&store, 12);
    for i in 0..crate::session_log::observed::PROGRAM_BURST {
        in_folder(&h, &format!("/srv/f{i}"));
        assert_eq!(pass(&store), None, "pass {i} has a token");
    }
    let last = format!("/srv/f{}", crate::session_log::observed::PROGRAM_BURST - 1);
    assert_eq!(
        derived(&h, Node::SESSION, TITLE).map(|t| t.0),
        Some(last.clone())
    );
    in_folder(&h, "/srv/owed");
    let wait = pass(&store).expect("the bucket is short");
    assert!(
        (1..=crate::session_log::observed::PROGRAM_REFILL_MS).contains(&wait),
        "owed until a token: {wait}"
    );
    in_folder(&h, "/srv/newest");
    assert!(pass(&store).is_some(), "still short");
    assert_eq!(
        derived(&h, Node::SESSION, TITLE).map(|t| t.0),
        Some(last),
        "nothing written while short"
    );
    hand.advance(crate::session_log::observed::PROGRAM_REFILL_MS);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&h, Node::SESSION, TITLE).map(|t| t.0),
        Some("/srv/newest".to_owned()),
        "the newest line is the one written"
    );
}

/// A REFUSED RUNG FALLS TO THE NEXT, A REFUSED DESCRIPTION LEAVES THE LAST: a folder that is only
/// a credential gives no title (the rung below it does, `shell`), and enters no record; a
/// description that is only a credential leaves T0's line standing on that subject, and where T0
/// holds none there the next line is written.
#[test]
fn a_refused_rung_falls_through_and_a_refused_description_leaves_the_last() {
    let token = format!("ghp_{}", "0123456789abcdefghij");
    assert!(
        crate::session_log::observed_stored(TITLE, &token, false, Actor::Aterm, Src::Aterm)
            .is_err(),
        "PRECONDITION: the folder reads as a credential"
    );
    let store = new_store();
    let (h, _hand) = registered(&store, 13);
    in_folder(&h, &token);
    assert_eq!(pass(&store), None);
    assert_eq!(
        shown(&h, TITLE).map(|l| (l.text, l.as_name)),
        Some((UNNAMED.to_owned(), None)),
        "the folder's rung is refused: the next one titles the session"
    );
    let claims = desc_records(&h);
    assert!(
        claims.iter().all(|r| !r.contains(&token)),
        "no claim holds the credential: {claims:?}"
    );

    let line = |text: &str| Line {
        on: Node::SESSION,
        text: text.to_owned(),
        rung: None,
        unread: false,
    };
    let lines = |description: Vec<Line>| Lines {
        title: vec![line(UNNAMED)],
        description,
    };
    let written = |lines: &Lines| {
        let mut g = h.ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        write(&mut g, lines, at)
    };
    assert_eq!(written(&lines(vec![line("Deploying the docs")])), Ok(1));
    assert_eq!(
        written(&lines(vec![line(&token), line("claude idle")])),
        Ok(0),
        "a refused description writes nothing where T0's line stands"
    );
    assert_eq!(
        derived(&h, Node::SESSION, DESCRIPTION).map(|d| d.0),
        Some("Deploying the docs".to_owned())
    );

    let (fresh, _fresh_hand) = registered(&store, 14);
    let mut g = fresh.ctx.timeline.lock().unwrap();
    let at = g.slog().now();
    assert_eq!(
        write(&mut g, &lines(vec![line(&token), line("claude idle")]), at),
        Ok(2)
    );
    drop(g);
    assert_eq!(
        derived(&fresh, Node::SESSION, DESCRIPTION).map(|d| d.0),
        Some("claude idle".to_owned()),
        "with no line of T0's to keep, the next is written"
    );
}

/// AN AGENT'S WORDS ARE TAKEN AT A TRIGGER, NOT AT EVERY WORD: words older than the trigger they
/// were taken at are owed, and the next move of the excerpts takes them once; a move after that
/// changes nothing until the next trigger, which takes them afresh.
///
/// CONTROL: a move of the excerpts that leaves their words as they were (their prompts moved)
/// leaves the words owed, and the move that brings new words takes them.
#[test]
fn an_agents_words_are_taken_at_a_trigger_and_once_after_one_they_lag() {
    let run = Node::parse("r2").expect("a run");
    let trigger = |t: u64| Trigger {
        turn: Some((Node::parse("t1").expect("a turn"), t)),
        needs: 0,
        agent: Some(run),
        run: None,
        last_run: None,
        task: None,
        cwd: 0,
        topic: None,
    };
    let seen = |excerpts: u64| Seen {
        ep: 0,
        rev: excerpts,
        seq: excerpts,
        excerpts,
        topic: None,
        task: 0,
        cwd: 0,
        cwd_unread: false,
    };
    let taken = |at: u64, end: &str, written: i64, excerpts: u64, said: &Said| Taken {
        read: Read {
            agent: Some(run),
            ..Read::default()
        },
        seen: seen(excerpts),
        trigger: trigger(at),
        trigger_at: at,
        finished: true,
        excerpts: Some(Excerpts {
            said_end: Some(end.into()),
            said_at_ms: Some(written),
            ..Excerpts::default()
        }),
        words_read: WordsRead::default(),
        cwd: None,
        said: said.clone(),
        want: Want::None,
    };
    let words = |read: &Read| read.words.as_ref().map(|(w, _)| w.clone());

    let (read, said) = taken(1_000, "Before the close", 500, 1, &Said::default()).settle(None, &[]);
    assert_eq!(words(&read).as_deref(), Some("Before the close"));
    assert!(said.owed, "words older than their trigger are owed");
    let (read, said) = taken(1_000, "Before the close", 500, 5, &said).settle(None, &[]);
    assert_eq!(words(&read).as_deref(), Some("Before the close"));
    assert!(
        said.owed,
        "CONTROL: a move that left the words as they were spends nothing"
    );
    let (read, said) = taken(1_000, "After the close", 1_500, 2, &said).settle(None, &[]);
    assert_eq!(
        words(&read).as_deref(),
        Some("After the close"),
        "taken once more"
    );
    assert!(!said.owed);
    let (read, said) = taken(1_000, "Still talking", 1_600, 3, &said).settle(None, &[]);
    assert_eq!(
        words(&read).as_deref(),
        Some("After the close"),
        "not at every word"
    );
    let (read, said) = taken(2_000, "Still talking", 2_100, 4, &said).settle(None, &[]);
    assert_eq!(
        words(&read).as_deref(),
        Some("Still talking"),
        "a new trigger"
    );
    assert!(!said.owed, "words newer than their trigger are not owed");
}

/// A DESCRIPTION THAT READS AS A PROTOCOL LINE FALLS THROUGH: the closing words of a turn under
/// Autopilot Land (`Nothing left: …`, the land check's `LANDED`) are refused, and the next line,
/// the agent's state, is written over T0's earlier line on the agent's run, which no longer holds.
///
/// CONTROL: the refusal is the description rule's protocol one, and a second such line beside the
/// same state writes nothing.
#[test]
fn a_description_that_reads_as_a_protocol_line_falls_to_the_next() {
    let store = new_store();
    let (h, _hand) = registered(&store, 15);
    let run = with_claude(&h);
    let line = |text: &str| Line {
        on: run,
        text: text.to_owned(),
        rung: None,
        unread: false,
    };
    let lines = |description: Vec<Line>| Lines {
        title: vec![line(UNNAMED)],
        description,
    };
    let written = |lines: &Lines| {
        let mut g = h.ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        write(&mut g, lines, at)
    };
    for protocol in ["Nothing left: landed abc123", "LANDED"] {
        assert_eq!(
            crate::session_log::observed_stored(
                DESCRIPTION,
                protocol,
                false,
                Actor::Aterm,
                Src::Aterm
            ),
            Err(DescRefused::Claim(aterm_slog::ClaimRefused::Protocol)),
            "CONTROL: {protocol:?} is refused as a protocol line"
        );
    }
    assert_eq!(
        written(&lines(vec![line("Running verify land now")])),
        Ok(2)
    );
    assert_eq!(
        written(&lines(vec![
            line("Nothing left: landed abc123"),
            line("claude idle")
        ])),
        Ok(1),
        "a protocol line falls to the agent's state"
    );
    assert_eq!(
        derived(&h, run, DESCRIPTION).map(|d| d.0),
        Some("claude idle".to_owned())
    );
    assert_eq!(
        written(&lines(vec![line("LANDED"), line("claude idle")])),
        Ok(0),
        "CONTROL: the same state again writes nothing"
    );
    assert_eq!(
        derived(&h, run, DESCRIPTION).map(|d| d.0),
        Some("claude idle".to_owned())
    );
}

/// A SEAMLESS UPDATE of `h` (registered in `store`), as the handoff runs it
/// (`session_log_tests::hand_over`): the manifest's draw, the worker's take of the log and the
/// sidecar's JSON round trip, the successor's adoption on a new process's clock `wall` ms into
/// the test (`spawn::mark_handoff_gap`), its engine reporting `cwd` as a restored checkpoint's
/// does, and that engine fitted to the log (`spawn::fit_adopted_log`) before the successor is
/// registered in a store of its own.
fn updated(
    h: &SessionHandle,
    store: &Store,
    wall: u64,
    cwd: Option<&str>,
) -> (SessionHandle, Store) {
    updated_with(h, store, wall, cwd, |_| {})
}

/// [`updated`], the carry's lines changed by `edit` on the wire.
fn updated_with(
    h: &SessionHandle,
    store: &Store,
    wall: u64,
    cwd: Option<&str>,
    edit: impl FnOnce(&mut Vec<String>),
) -> (SessionHandle, Store) {
    let manifest = SessionHandoff::from_store(&store.read().unwrap());
    let read = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
    let rec = read
        .sessions
        .iter()
        .find(|r| r.local_id == h.local_id)
        .expect("the record")
        .clone();
    let out = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog_carry_source()
        .encode(aterm_slog::CarryBudget::SESSION)
        .expect("carried");
    let mut wire: Vec<String> =
        aterm_json::from_slice(&aterm_json::to_vec(&out.lines).unwrap()).unwrap();
    edit(&mut wire);
    let successor = test_handle(h.local_id);
    let _hand = hand_clock_from(&successor.ctx.timeline, wall);
    crate::spawn::mark_handoff_gap(
        &successor.ctx.timeline,
        &successor.ctx.cast,
        &successor.ctx.temporal,
        Some(7),
        rec.timeline_id,
        LogAdopt {
            ids: rec.log_ids.as_deref().and_then(LogCarry::parse),
            seq: rec.log_seq,
            lines: Some(wire),
            keeper_ep: None,
            mirror: None,
        },
        false,
    );
    if let Some(cwd) = cwd {
        crate::term_lock(&successor.term)
            .process(format!("\x1b]7;file://localhost{cwd}\x07").as_bytes());
    }
    crate::spawn::fit_adopted_log(&successor.term, &successor.ctx.timeline);
    let next = new_store();
    next.write().unwrap().register(successor.clone());
    (successor, next)
}

/// A CARRIED CLAIM IS NOT REWRITTEN AFTER AN UPDATE: the folder and an agent's words live only in
/// memory, so the successor reads its adopted engine's folder before the session is registered,
/// and keeps the agent's description the update carried until it has read the agent's words
/// again. A pass before any read appends nothing; the drift flush's read of the same folder
/// appends nothing and its pass nothing; the first excerpt read, though it moves what a pass
/// reads, writes nothing where the words are the ones carried; a folder that moved is written.
///
/// CONTROLS: a successor whose engine reports no folder keeps the carried title as well, until
/// a line written in this process stands (the task's line): then the hold, which keeps only what
/// the update carried, is gone, and the next line that rests on the unread folder is written; the
/// agents aterm reads the words of are the ones the hold waits for.
#[test]
fn a_carried_claim_is_not_rewritten_after_an_update() {
    let store = new_store();
    let (shell, _shell_hand) = registered(&store, 21);
    in_folder(&shell, FOLDER);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&shell, Node::SESSION, TITLE),
        Some((FOLDER.to_owned(), Some("folder".to_owned()))),
        "PRECONDITION: T0's folder title"
    );
    let (shell2, next) = updated(&shell, &store, 10_000, Some(FOLDER));
    let seq = last_seq(&shell2);
    assert_eq!(pass(&next), None);
    assert_eq!(
        last_seq(&shell2),
        seq,
        "a pass before any read appends nothing"
    );
    assert_eq!(
        shown(&shell2, TITLE).map(|l| (l.text, l.as_name)),
        Some((FOLDER.to_owned(), Some("folder".to_owned())))
    );
    assert!(
        !shell2
            .ctx
            .timeline
            .lock()
            .unwrap()
            .record_cwd_change(Some(FOLDER)),
        "the drift flush's read of the same folder: no mark"
    );
    assert_eq!(pass(&next), None);
    assert_eq!(last_seq(&shell2), seq, "and its pass appends nothing");
    in_folder(&shell2, "/srv/elsewhere");
    assert_eq!(pass(&next), None);
    assert_eq!(
        shown(&shell2, TITLE).map(|l| (l.text, l.as_name)),
        Some(("/srv/elsewhere".to_owned(), Some("folder".to_owned()))),
        "a folder that moved is written"
    );

    let store = new_store();
    let (agent, _agent_hand) = registered(&store, 22);
    in_folder(&agent, FOLDER);
    let run = with_claude(&agent);
    said(&agent, "Running the tests now.", CLOSING);
    assert_eq!(pass(&store), None);
    let agent_title = format!("claude · {FOLDER}");
    assert_eq!(
        derived(&agent, run, TITLE).map(|t| t.0),
        Some(agent_title.clone())
    );
    assert_eq!(
        derived(&agent, run, DESCRIPTION).map(|d| d.0),
        Some(CLOSING.to_owned()),
        "PRECONDITION: the agent's closing words"
    );
    let (agent2, next) = updated(&agent, &store, 10_000, Some(FOLDER));
    let seq = last_seq(&agent2);
    assert_eq!(pass(&next), None);
    assert_eq!(
        last_seq(&agent2),
        seq,
        "a pass before the agent's words are read appends nothing: {:?}",
        desc_records(&agent2)
    );
    assert_eq!(
        shown(&agent2, DESCRIPTION).map(|l| (l.text, l.scope)),
        Some((CLOSING.to_owned(), run))
    );
    said(&agent2, "Running the tests now.", CLOSING);
    assert_eq!(pass(&next), None);
    assert_eq!(
        last_seq(&agent2),
        seq,
        "the words read again are the ones carried: nothing written"
    );
    assert_eq!(
        shown(&agent2, TITLE).map(|l| (l.text, l.scope)),
        Some((agent_title, run))
    );

    let store = new_store();
    let (unread, _unread_hand) = registered(&store, 24);
    in_folder(&unread, FOLDER);
    assert_eq!(pass(&store), None);
    let (blind, next) = updated(&unread, &store, 10_000, None);
    let seq = last_seq(&blind);
    assert_eq!(pass(&next), None);
    assert_eq!(
        last_seq(&blind),
        seq,
        "CONTROL: an engine with no folder: the carried title stands"
    );
    assert_eq!(
        shown(&blind, TITLE).map(|l| (l.text, l.as_name)),
        Some((FOLDER.to_owned(), Some("folder".to_owned())))
    );
    tasked(&blind);
    assert_eq!(pass(&next), None);
    assert_eq!(
        shown(&blind, TITLE).map(|l| (l.text, l.as_name)),
        Some((TASK_LINE.to_owned(), Some("task".to_owned())))
    );
    blind.ctx.timeline.lock().unwrap().set_task_line(None);
    assert_eq!(pass(&next), None);
    assert_eq!(
        shown(&blind, TITLE).map(|l| (l.text, l.as_name)),
        Some((UNNAMED.to_owned(), None)),
        "CONTROL: a line of this process's is not held"
    );

    assert!(
        words_read_for("claude") && words_read_for("codex") && !words_read_for("grok"),
        "CONTROL: the hold waits only for words aterm reads"
    );
}

/// THE FOLDER'S TEXT ARRIVES WITH NO RECORD AFTER AN UPDATE, AND THE NEXT PASS READS IT: a carried
/// log already holds the folder's fingerprint, so the drift flush's first read of it after an
/// update appends no mark, and only the timeline's ring tells T0 (inert in tests); the pass that
/// ring starts takes the session, whose folder moved, and writes the folder's title over the
/// `shell` it wrote before the folder was read, where T0 held no line the update carried.
#[test]
fn a_folder_read_with_no_record_after_an_update_is_read_at_the_next_pass() {
    let store = new_store();
    let (h, _hand) = registered(&store, 23);
    in_folder(&h, FOLDER);
    let (s, next) = updated(&h, &store, 10_000, None);
    assert_eq!(pass(&next), None);
    assert_eq!(
        shown(&s, TITLE).map(|l| (l.text, l.as_name)),
        Some((UNNAMED.to_owned(), None)),
        "PRECONDITION: no folder known and no line of T0's carried"
    );
    let seq = last_seq(&s);
    let before = s.ctx.timeline.lock().unwrap().described().seen;
    assert!(
        !s.ctx
            .timeline
            .lock()
            .unwrap()
            .record_cwd_change(Some(FOLDER)),
        "the fingerprint is the carried one: no mark"
    );
    assert_eq!(last_seq(&s), seq, "and no record");
    assert_eq!(pass(&next), None);
    assert_ne!(
        s.ctx.timeline.lock().unwrap().described().seen,
        before,
        "the pass took the session"
    );
    assert_eq!(
        shown(&s, TITLE).map(|l| (l.text, l.as_name)),
        Some((FOLDER.to_owned(), Some("folder".to_owned())))
    );
}

/// AN EXCERPT RE-READ THAT MOVED NOTHING TELLS T0 NOTHING: the footer resolver re-reads on its idle
/// clocks, and only a move of what a pass reads of the excerpts (their words, their row's time,
/// their presence) rings the lane, so an idle Mac runs no pass for it. The first read after an
/// update moves it even where it found none, so the words held back are read.
#[test]
fn an_excerpt_reread_that_moved_nothing_rings_nothing() {
    let excerpts = |at: Option<i64>, doing: &str| {
        Some(ClaudeExcerpts {
            excerpts: Excerpts {
                said: Some("Running the tests.".into()),
                said_at_ms: at,
                doing: Some(doing.into()),
                ..Excerpts::default()
            },
            ..ClaudeExcerpts::default()
        })
    };
    let mut tl = SessionTimeline::default();
    let pgid = tl.agent().program_pgid;
    assert!(!tl.set_claude_excerpts(pgid, None), "none read, none held");
    assert!(tl.set_claude_excerpts(pgid, excerpts(None, "Bash")), "read");
    assert!(
        !tl.set_claude_excerpts(pgid, excerpts(None, "Bash")),
        "the same read again"
    );
    assert!(
        tl.set_claude_excerpts(pgid, excerpts(Some(5), "Bash")),
        "the same words at a new row time"
    );
    assert!(
        tl.set_claude_excerpts(pgid, excerpts(Some(5), "Read")),
        "a new `doing`"
    );
    assert!(
        !tl.set_claude_excerpts(pgid + 1, excerpts(Some(6), "Edit")),
        "CONTROL: another group's are not taken"
    );
    assert!(tl.set_claude_excerpts(pgid, None), "gone");

    tl.note_unread_after_update();
    assert!(
        tl.set_claude_excerpts(pgid, None),
        "the first read after an update, though it found none"
    );
    assert!(!tl.words_unread());
}

/// THE LANE IS INERT UNPUBLISHED: no test publishes it, so a ring does nothing, no session log is
/// given its callback, and the lane stays unpublished.
#[test]
fn ring_with_the_lane_unpublished_does_nothing() {
    assert!(LANE.get().is_none(), "no test publishes the lane");
    ring();
    ring();
    assert!(LANE.get().is_none());
    assert!(notify().is_none(), "no log is given the lane's callback");
}

/// The source of the title a reader is shown of `h` ([`aterm_slog::title_src`]).
fn title_src_of(h: &SessionHandle) -> Option<TitleSrc> {
    let g = h.ctx.timeline.lock().unwrap();
    let now = g.slog().now().wall_ms;
    aterm_slog::title_src(g.slog().log().state(), now)
}

/// The first prompt `h`'s agent run `run` keeps.
fn first_prompt(h: &SessionHandle, run: Node) -> Option<String> {
    let g = h.ctx.timeline.lock().unwrap();
    g.slog().first_prompt(run).map(str::to_owned)
}

/// AN AGENT IN AN OLD SHELL IS TITLED BY ITS FIRST PROMPT (CX-04's exit test): Claude Code started
/// by hand in a shell, with no task, whose excerpts hold `keep going` and, older, `wire the
/// survey`. One pass titles its run by the oldest prompt the harness did not type, as aterm's
/// derived claim on the run (`as=prompt`, read `title_src=prompt`), and the run keeps that prompt.
///
/// CONTROLS: (a) later excerpts that no longer hold it, and a folder that moved, leave the title
/// unchanged; (b) with the harness's own turn typing `wire the survey`, the title is `keep
/// going`; (c) with a task, the title is the task's line on the session and nothing is kept; (d)
/// once the agent ends, a new run is titled by its own first prompt; (e) with no excerpts the
/// title is `claude · <folder>`, `as=program`; (f) a prompt that is only a credential falls to
/// `as=program` too; (g) a bash-mode command (`! git status`) is no first prompt: alone it titles
/// nothing and nothing is kept, and the typed prompt after it is the first, while a typed prompt
/// whose text starts with `! ` is one (the rule reads the entry's kind, not its text); (h) a
/// prompt that opens with one long token, which the excerpt's cap leaves as a lone `…`, falls to
/// `as=program` and is kept, so the prompt after it is never picked.
#[test]
fn an_agent_in_an_old_shell_is_titled_by_its_first_prompt() {
    let store = new_store();
    let (h, _hand) = registered(&store, 21);
    in_folder(&h, FOLDER);
    let run = with_claude(&h);
    prompted(&h, &["keep going", "wire the survey"]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&h, run, TITLE),
        Some(("wire the survey".to_owned(), Some("prompt".to_owned())))
    );
    let title = shown(&h, TITLE).expect("a title stands");
    assert_eq!(
        (title.text.as_str(), title.scope, title.rank),
        ("wire the survey", run, Rank::Derived)
    );
    assert_eq!(title_src_of(&h), Some(TitleSrc::Prompt));
    assert_eq!(first_prompt(&h, run).as_deref(), Some("wire the survey"));

    // (a) The excerpts move on and the folder moves: the kept prompt still titles the run.
    prompted(&h, &["ship it", "keep going"]);
    in_folder(&h, "/srv/elsewhere");
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&h, run, TITLE).map(|t| t.0).as_deref(),
        Some("wire the survey"),
        "CONTROL (a): the first prompt is kept"
    );

    // (b) The harness typed the older prompt.
    let (typed, _typed_hand) = registered(&store, 22);
    in_folder(&typed, FOLDER);
    let typed_run = with_claude(&typed);
    typed
        .ctx
        .turns
        .lock()
        .unwrap()
        .push(crate::turn_ledger::TurnRecord {
            id: 1,
            started_ms: 0,
            dur_ms: 1,
            submitted: true,
            status: "settled",
            text: "wire the survey".into(),
            screen_hash: 0,
            seq: 0,
            arch: Default::default(),
            by: crate::turn_ledger::TurnBy::Harness,
            carried: false,
        });
    prompted(&typed, &["keep going", "wire the survey"]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&typed, typed_run, TITLE).map(|t| t.0).as_deref(),
        Some("keep going"),
        "CONTROL (b): a prompt the harness typed is no person's first"
    );

    // (c) A session with a task is titled by its task's line.
    let (tasked_h, _tasked_hand) = registered(&store, 23);
    in_folder(&tasked_h, FOLDER);
    let tasked_run = with_claude(&tasked_h);
    tasked(&tasked_h);
    prompted(&tasked_h, &["keep going", "wire the survey"]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        shown(&tasked_h, TITLE).map(|l| (l.text, l.scope, l.as_name)),
        Some((TASK_LINE.to_owned(), Node::SESSION, Some("task".to_owned()))),
        "CONTROL (c)"
    );
    assert_eq!(
        first_prompt(&tasked_h, tasked_run),
        None,
        "CONTROL (c): nothing kept"
    );

    // (d) A new run after the first ends.
    agent_ends(&h, run);
    let next = with_claude(&h);
    assert_ne!(next, run, "PRECONDITION: a new agent run");
    prompted(&h, &["review the diff"]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&h, next, TITLE),
        Some(("review the diff".to_owned(), Some("prompt".to_owned()))),
        "CONTROL (d)"
    );

    // (e) No excerpts: the program and the folder.
    let (bare, _bare_hand) = registered(&store, 24);
    in_folder(&bare, FOLDER);
    let bare_run = with_claude(&bare);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&bare, bare_run, TITLE),
        Some((format!("claude · {FOLDER}"), Some("program".to_owned()))),
        "CONTROL (e)"
    );

    // (f) A prompt that is only a credential.
    let token = format!("ghp_{}", "0123456789abcdefghij");
    let (secret, _secret_hand) = registered(&store, 25);
    in_folder(&secret, FOLDER);
    let secret_run = with_claude(&secret);
    prompted(&secret, &[token.as_str()]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&secret, secret_run, TITLE),
        Some((format!("claude · {FOLDER}"), Some("program".to_owned()))),
        "CONTROL (f): a refused prompt falls to the next rung"
    );
    assert!(
        desc_records(&secret).iter().all(|r| !r.contains(&token)),
        "CONTROL (f): no claim holds the credential"
    );

    // (g) A bash-mode command is no first prompt; a typed `! ` is.
    let (bash, _bash_hand) = registered(&store, 26);
    in_folder(&bash, FOLDER);
    let bash_run = with_claude(&bash);
    prompted_kinds(&bash, &[("! git status", true)]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&bash, bash_run, TITLE),
        Some((format!("claude · {FOLDER}"), Some("program".to_owned()))),
        "CONTROL (g): a command line titles nothing"
    );
    assert_eq!(
        first_prompt(&bash, bash_run),
        None,
        "CONTROL (g): nothing kept"
    );
    prompted_kinds(
        &bash,
        &[("fix the failing test", false), ("! git status", true)],
    );
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&bash, bash_run, TITLE),
        Some(("fix the failing test".to_owned(), Some("prompt".to_owned()))),
        "CONTROL (g): the typed prompt after it is the first"
    );
    assert_eq!(title_src_of(&bash), Some(TitleSrc::Prompt));
    assert_eq!(
        first_prompt(&bash, bash_run).as_deref(),
        Some("fix the failing test")
    );
    let (bang, _bang_hand) = registered(&store, 27);
    in_folder(&bang, FOLDER);
    let bang_run = with_claude(&bang);
    prompted(&bang, &["! check the bang handling"]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&bang, bang_run, TITLE),
        Some((
            "! check the bang handling".to_owned(),
            Some("prompt".to_owned())
        )),
        "CONTROL (g): the kind decides, not the text"
    );

    // (h) A prompt the excerpt's cap leaves as a lone `…`.
    let capped = aterm_agent::harness::excerpt::cap(
        &aterm_agent::harness::excerpt::fold(&format!("{} why does this fail?", "a".repeat(600))),
        aterm_agent::harness::excerpt::EXCERPT_BYTES,
    );
    assert_eq!(capped, "…", "PRECONDITION: the cap backs out of the token");
    let (long, _long_hand) = registered(&store, 28);
    in_folder(&long, FOLDER);
    let long_run = with_claude(&long);
    prompted(&long, &[capped.as_str()]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&long, long_run, TITLE),
        Some((format!("claude · {FOLDER}"), Some("program".to_owned()))),
        "CONTROL (h): a prompt with no word titles nothing"
    );
    assert_eq!(first_prompt(&long, long_run).as_deref(), Some("…"));
    prompted(&long, &["fix the build", capped.as_str()]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&long, long_run, TITLE),
        Some((format!("claude · {FOLDER}"), Some("program".to_owned()))),
        "CONTROL (h): the prompt after it is not the first"
    );
}

/// THE PROMPTS ALONE MOVING RING T0 WHILE A TITLE WANTS ONE: for Claude Code started by hand in
/// a shell (no task, no prompt kept), an excerpt read that moved only the prompts moves what T0
/// reads ([`super::excerpts_seen`], what the setters ring the lane on and a pass compares), so a
/// person's first prompt titles the run at once, even behind one the harness typed, whose key
/// alone would read the same.
///
/// CONTROLS: the same prompts read again, a prompt that is a bash-mode command, a prompt after
/// one was kept, and a prompt to a session with a task move nothing T0 reads, so none rings.
#[test]
fn the_prompts_alone_moving_ring_t0_while_a_title_wants_one() {
    let store = new_store();
    let (h, _hand) = registered(&store, 31);
    in_folder(&h, FOLDER);
    let run = with_claude(&h);
    assert_eq!(pass(&store), None);
    assert!(prompted(&h, &[]), "PRECONDITION: the first read moves it");
    assert!(!prompted(&h, &[]), "CONTROL: the same read again");
    assert!(
        !prompted_kinds(&h, &[("! git status", true)]),
        "CONTROL: a command line is nothing T0 reads"
    );
    assert!(
        prompted(&h, &["wire the survey"]),
        "the prompts alone moved: the lane is rung"
    );
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&h, run, TITLE),
        Some(("wire the survey".to_owned(), Some("prompt".to_owned())))
    );
    assert!(
        !prompted(&h, &["keep going", "wire the survey"]),
        "CONTROL: once a prompt is kept the prompts move nothing"
    );

    let (tasked_h, _tasked_hand) = registered(&store, 32);
    in_folder(&tasked_h, FOLDER);
    let _ = with_claude(&tasked_h);
    tasked(&tasked_h);
    assert!(prompted(&tasked_h, &[]), "PRECONDITION: the first read");
    assert!(
        !prompted(&tasked_h, &["wire the survey"]),
        "CONTROL: with a task the prompts move nothing"
    );

    // Behind a prompt the harness typed: the oldest key reads the same.
    let (typed, _typed_hand) = registered(&store, 33);
    in_folder(&typed, FOLDER);
    let typed_run = with_claude(&typed);
    typed
        .ctx
        .turns
        .lock()
        .unwrap()
        .push(crate::turn_ledger::TurnRecord {
            id: 1,
            started_ms: 0,
            dur_ms: 1,
            submitted: true,
            status: "settled",
            text: "H1".into(),
            screen_hash: 0,
            seq: 0,
            arch: Default::default(),
            by: crate::turn_ledger::TurnBy::Harness,
            carried: false,
        });
    prompted(&typed, &["H1"]);
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&typed, typed_run, TITLE),
        Some((format!("claude · {FOLDER}"), Some("program".to_owned()))),
        "PRECONDITION: the harness's prompt titles nothing"
    );
    assert!(
        prompted(&typed, &["P2", "H1"]),
        "a person's prompt behind it"
    );
    assert_eq!(pass(&store), None);
    assert_eq!(
        derived(&typed, typed_run, TITLE),
        Some(("P2".to_owned(), Some("prompt".to_owned())))
    );
    assert_eq!(first_prompt(&typed, typed_run).as_deref(), Some("P2"));
}

/// A REFUSED FIRST PROMPT IS NOT PICKED AGAIN AFTER AN UPDATE: a run whose first prompt no claim
/// could hold (`fix the password reset flow` reads as a credential) is titled `claude · <folder>`
/// and keeps that prompt; a seamless update carries the fact that it kept one (the agent line's
/// `kept=1`), never its text, so once the excerpts are read again, holding only later prompts,
/// a pass writes no title and the carried one stands.
///
/// CONTROL: the same carry with `kept=1` taken off its agent line picks the oldest of the later
/// prompts, which is not the run's first.
#[test]
fn a_refused_first_prompt_is_not_picked_again_after_an_update() {
    let later: Vec<String> = (2..=11).map(|n| format!("step {n} of the plan")).collect();
    // The newest 8 an excerpt keeps, newest first: steps 11 to 4.
    let kept_8: Vec<&str> = later.iter().rev().take(8).map(String::as_str).collect();
    let program = || Some((format!("claude · {FOLDER}"), Some("program".to_owned())));
    let titles = |h: &SessionHandle| {
        desc_records(h)
            .into_iter()
            .filter(|r| r.contains(" field=title"))
            .count()
    };
    for carried_kept in [true, false] {
        let store = new_store();
        let (h, _hand) = registered(&store, 41);
        in_folder(&h, FOLDER);
        let run = with_claude(&h);
        prompted(&h, &["fix the password reset flow"]);
        assert_eq!(pass(&store), None);
        assert_eq!(derived(&h, run, TITLE), program(), "PRECONDITION: refused");
        assert_eq!(
            first_prompt(&h, run).as_deref(),
            Some("fix the password reset flow"),
            "PRECONDITION: kept"
        );
        let mut all: Vec<&str> = later.iter().rev().map(String::as_str).collect();
        all.push("fix the password reset flow");
        prompted(&h, &all[..8]);
        assert_eq!(pass(&store), None);
        assert_eq!(derived(&h, run, TITLE), program());

        let (next, next_store) = updated_with(&h, &store, 10_000, Some(FOLDER), |lines| {
            if carried_kept {
                return;
            }
            let mut agent = lines.iter_mut().filter(|l| l.contains("agent pgid="));
            let line = agent.next().expect("PRECONDITION: the carry's agent line");
            assert!(line.contains(" kept=1"), "PRECONDITION: {line}");
            *line = line.replace(" kept=1", "");
            assert!(agent.next().is_none(), "PRECONDITION: one agent line");
        });
        let next_run = {
            let g = next.ctx.timeline.lock().unwrap();
            assert_eq!(g.slog().kept_first_prompt(run), carried_kept);
            g.slog().log().state().desc.open_agent()
        };
        assert_eq!(next_run, Some(run), "PRECONDITION: the run is carried");
        let before = titles(&next);
        prompted(&next, &kept_8);
        assert_eq!(pass(&next_store), None);
        if carried_kept {
            assert_eq!(titles(&next), before, "no title is written");
            assert_eq!(derived(&next, run, TITLE), program());
        } else {
            assert_eq!(
                derived(&next, run, TITLE),
                Some(("step 4 of the plan".to_owned(), Some("prompt".to_owned()))),
                "CONTROL: with no kept=, a later prompt is taken for the first"
            );
        }
    }
}
