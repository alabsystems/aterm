// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ATERM'S DEFAULT WRITER, T0 (streams design §6, comprehension design §3.1): with no model, no
//! child process and no file read, it gives every session a `title` and a `description` claim
//! from what aterm already observes, at the lowest rank that stands
//! ([`aterm_slog::Rank::Derived`]: `by=aterm src=aterm conf=heuristic`). So no slot is empty, and
//! every other writer only has to be better.
//!
//! The title is the first rung that gives a line ([`Rung`]): the task's first line (ST-02), an
//! agent's first prompt the harness did not type (CX-04), the program and the folder, the folder
//! alone, the program alone, else `shell`. It is never a command line: the program is a word of
//! the fold's runs (`prog=`, `detail=`, `agent=`), never the terminal's title, and a prompt entry
//! that is a command the person ran (Claude Code's bash mode, `! cmd`) is never a first prompt,
//! by the entry's kind ([`Prompt::shell`]), not its text. The description is, for an agent, its
//! open need's words, else its newest topic post where that is newer than its last words, else
//! those words, else whether it is working; for a shell or a plain program, what runs (or
//! `idle`), then the last command's word and how it ended. No line holds an age or a duration: a
//! line moves only when what it says does.
//!
//! What aterm observes of a session's program is about the open agent run while one is open,
//! else about the session, as an in-band title is: the description and the title's prompt,
//! program and folder rungs are claimed on `r<n>` while an agent runs and on `-` otherwise, and
//! the task's line, the session's task, on `-` whatever runs. When a slot's subject moves, T0
//! first withdraws its claim of that slot on the other subject: [`aterm_slog::shown`] gives a
//! tie to the session, which would hide the agent's line.
//!
//! A line is written only where it differs from T0's own claim, so a pass over what did not move
//! writes nothing. After a seamless update the folder and an agent's words, which live only in
//! memory, are unread until this process reads them again (the adopted engine's folder is read
//! before the session is registered); until then a line that rests on one never replaces the
//! claim the update carried.
//!
//! A thread, `aterm-describe`, runs the passes ([`pass`]). It is rung ([`ring`]) by the one
//! callback each session log rings on every append (a topic post's among them: its `mail
//! dir=out` mark is appended once the post is noted), and by the writes that move what a pass
//! reads with no record of their own: an agent's excerpts or a codex survey's where what a pass
//! reads of them moved ([`excerpts_seen`]: their words and, while a run's title wants its first
//! prompt, their prompts), a task's line, and the folder's text where the log's fingerprint of it
//! did not move (a carried log already holds it). It sleeps unrung with no timer, so an idle Mac
//! runs no pass. Rung, it runs one pass and holds the next to at least [`PASS_GAP`] after the
//! last one started; its one deadline is armed only while a ring or a session it could not finish
//! is owed. Only the ring that finds the lane unrung wakes the thread: the flag stays set until a
//! pass starts, so the rings of a held pass cost an atomic each. Only the real app's startup
//! starts it: tests run [`pass`] themselves, and nothing is ever written inside a producer or a
//! register.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, TryLockError};
use std::time::{Duration, Instant};

use aterm_agent::harness::excerpt::{Excerpts, Prompt};
use aterm_hash::Fnv1a64;
use aterm_slog::{
    Actor, ClaimRefused, Conf, Described, Key, Kind, Node, OpenNode, Src, Stamp, TitleSrc,
};

use crate::fabric::TopicPost;
use crate::session_log::observed::MarkBucket;
use crate::session_log::{DescRefused, ObservedClaim, observed_stored};
use crate::session_timeline::SessionTimeline;
use crate::survey::WordsRead;

/// The slot of what the session is called.
pub(crate) const TITLE: &str = "title";
/// The slot of what the session is doing and where it stands.
pub(crate) const DESCRIPTION: &str = "description";
/// The title where nothing else is known: the session's shell, at its prompt.
pub(crate) const UNNAMED: &str = "shell";
/// A shell's, or an agent's, word for nothing running.
const IDLE: &str = "idle";
/// The author of every claim T0 makes, with [`SRC`]: [`aterm_slog::Rank::Derived`].
const BY: Actor = Actor::Aterm;
const SRC: Src = Src::Aterm;
const CONF: Conf = Conf::Heuristic;
/// The least time from one pass's start to the next's.
pub(crate) const PASS_GAP: Duration = Duration::from_millis(250);

/// WHERE A TITLE CAME FROM, its claim's `as=` (the comprehension design's `title_src`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rung {
    /// The task's first line (ST-02).
    Task,
    /// An agent's first prompt the harness did not type (CX-04).
    Prompt,
    /// The program: with the folder, or alone where no folder is known.
    Program,
    /// The folder alone.
    Folder,
}

impl Rung {
    /// The `title_src` word a claim from this rung reads as ([`aterm_slog::TitleSrc::of`]).
    pub(crate) const fn src(self) -> TitleSrc {
        match self {
            Rung::Task => TitleSrc::Task,
            Rung::Prompt => TitleSrc::Prompt,
            Rung::Program => TitleSrc::Program,
            Rung::Folder => TitleSrc::Folder,
        }
    }

    /// The claim's `as=` word: its `title_src` word, so the words exist in one place.
    pub(crate) const fn word(self) -> &'static str {
        self.src().word()
    }
}

/// How the newest closed command ended, as the fold names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    /// It exited with this code (`ok` with 0, `fail` with any other).
    Exit(i64),
    /// A signal ended it (`out=signal`).
    Signal,
    /// Nobody knows how (`out=unknown`, no code).
    Unknown,
}

/// WHAT ONE SESSION'S PASS DERIVES FROM ([`derive`]): a plain copy, taken under the session's
/// timeline lock alone and from its fabric's last topic post with no other lock held, so it is
/// worked out with no lock held at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Read {
    /// The open agent run (the newest agent run still open): the subject of everything aterm
    /// observes of the session's program while it runs.
    pub(crate) agent: Option<Node>,
    /// The program's word: the open agent's (`agent=`) while one runs, else the deepest open
    /// run's program (`prog=`) or command (`detail=`); `None` while a shell sits at its prompt,
    /// or where the run's word is not yet confirmed.
    pub(crate) program: Option<String>,
    /// Whether any run is open: a command, a program, an agent.
    pub(crate) running: bool,
    /// Whether a turn is open.
    pub(crate) turn_open: bool,
    /// The open need's words ([`aterm_slog::need_words`] of the most urgent need), never its age.
    pub(crate) need: Option<String>,
    /// The newest closed run, where it is a command with a confirmed word: that word, and how it
    /// ended.
    pub(crate) last_cmd: Option<(String, Ended)>,
    /// The task's first line ([`crate::task_record::TaskRecord::line`], ST-02's rung).
    pub(crate) task_line: Option<String>,
    /// The open agent run's first prompt the harness did not type
    /// ([`crate::session_log::SessionLog::first_prompt`]); `None` with a task record or with no
    /// agent run. One with no word (no alphanumeric character, such as the excerpt cap's lone
    /// `…` for a prompt that opens with one long token) titles nothing: the title takes its next
    /// rung, as for a prompt the description rule refuses.
    pub(crate) prompt: Option<String>,
    /// The folder, home-abbreviated ([`crate::app_tabs::home_abbreviated`]).
    pub(crate) folder: Option<String>,
    /// The agent's last words as of the newest trigger, redacted, with when they were written
    /// (wall ms): [`crate::survey::excerpt_last_words`], re-read only at a trigger and once
    /// after one that came before its words ([`Said`]).
    pub(crate) words: Option<(String, u64)>,
    /// The newest topic post's words ([`crate::survey::topic_words`]), with when it was posted
    /// (wall ms).
    pub(crate) topic: Option<(String, u64)>,
    /// Whether this process has yet to read the folder since a seamless update adopted the
    /// session ([`SessionTimeline::cwd_unread`]): every title but the task's line rests on it.
    pub(crate) folder_unread: bool,
    /// Whether this process has yet to read the open agent's words since the update, where aterm
    /// reads that agent's words at all ([`words_read_for`]): `<program> working|idle` rests on
    /// them.
    pub(crate) words_unread: bool,
}

/// One line T0 would claim of one slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    /// The subject: the session, or its open agent run.
    pub(crate) on: Node,
    /// The text: one line, no hidden character, within the slot's bound ([`line_of`]).
    pub(crate) text: String,
    /// For a title, the rung it came from; `None` for the constant and every description.
    pub(crate) rung: Option<Rung>,
    /// Whether it rests on an input this process has yet to read since a seamless update
    /// ([`Read::folder_unread`], [`Read::words_unread`]): such a line never replaces a claim of
    /// T0's the update carried, which stands until the input is read.
    pub(crate) unread: bool,
}

/// WHAT T0 DERIVED of one session: each slot's lines, best first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lines {
    /// The title's rungs that give a line, [`UNNAMED`] last: the first the description rule
    /// admits is written, so a refused rung falls to the next.
    pub(crate) title: Vec<Line>,
    /// The description, then what it falls back to: the first the rule admits is written, except
    /// that where the first is refused for its form (only a credential, or nothing once cleaned)
    /// T0's line already standing on that subject stays; any other refusal (a protocol line)
    /// falls to the next.
    pub(crate) description: Vec<Line>,
}

/// `text` as one line a claim may hold: hidden characters dropped as
/// [`crate::task_record::task_line`] drops them (a hidden blank becomes a space), every run of
/// blanks one space, cut to `max` encoded bytes at whole graphemes
/// ([`crate::task_record::cut_to_encoded`]) and, where that cut a word, back to the last whole
/// one. `None` where nothing is left.
pub(crate) fn line_of(text: &str, max: usize) -> Option<String> {
    let shown: String = text
        .chars()
        .filter_map(|c| {
            if !crate::session_timeline::is_forbidden_metadata_char(c) {
                Some(c)
            } else if c.is_whitespace() {
                Some(' ')
            } else {
                None
            }
        })
        .collect();
    let one = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = crate::task_record::cut_to_encoded(&one, max);
    let cut = if cut.len() < one.len() && !one[cut.len()..].starts_with(' ') {
        match cut.rfind(' ') {
            Some(at) => cut[..at].trim_end().to_owned(),
            None => cut,
        }
    } else {
        cut
    };
    (!cut.is_empty()).then_some(cut)
}

/// THE LINES OF `read`: pure, with no clock, so no line holds an age or a duration.
pub(crate) fn derive(read: &Read) -> Lines {
    let subject = read.agent.unwrap_or(Node::SESSION);
    let lines = |slot: &str, cands: Vec<(Node, String, Option<Rung>, bool)>| -> Vec<Line> {
        let max = aterm_slog::slot_bound(slot);
        cands
            .into_iter()
            .filter_map(|(on, text, rung, unread)| {
                line_of(&text, max).map(|text| Line {
                    on,
                    text,
                    rung,
                    unread,
                })
            })
            .collect()
    };
    let unread = read.folder_unread;
    let mut title = Vec::with_capacity(6);
    if let Some(task) = &read.task_line {
        title.push((Node::SESSION, task.clone(), Some(Rung::Task), false));
    }
    // A prompt with no word says nothing the title could: its rung is refused, as a
    // credential's is, and the run still keeps it, so no later prompt is taken for its first.
    if let (Some(agent), Some(prompt)) = (read.agent, &read.prompt)
        && prompt.chars().any(char::is_alphanumeric)
    {
        title.push((agent, prompt.clone(), Some(Rung::Prompt), false));
    }
    if let (Some(program), Some(folder)) = (&read.program, &read.folder) {
        title.push((
            subject,
            format!("{program} · {folder}"),
            Some(Rung::Program),
            unread,
        ));
    }
    if let Some(folder) = &read.folder {
        title.push((subject, folder.clone(), Some(Rung::Folder), unread));
    }
    if let Some(program) = &read.program {
        title.push((subject, program.clone(), Some(Rung::Program), unread));
    }
    title.push((subject, UNNAMED.to_owned(), None, unread));
    let description = description_texts(read)
        .into_iter()
        .map(|(text, unread)| (subject, text, None, unread))
        .collect();
    Lines {
        title: lines(TITLE, title),
        description: lines(DESCRIPTION, description),
    }
}

/// The description's texts, best first ([`Lines::description`]), each with whether it rests on
/// an unread input ([`Line::unread`]). An agent's: its open need's words; else its newest topic
/// post where that is newer than its words or it has none; else its words; and last whether it
/// is working, by the fold's open turn, which stands for words not yet read. A shell's or a
/// plain program's: `running <word>` while a run is open, else `idle`, then, where the newest
/// closed run is a command with a word, ` · <word> exit <code>`, or ` · <word> signal`.
fn description_texts(read: &Read) -> Vec<(String, bool)> {
    if read.agent.is_some() {
        let words = read.words.as_ref();
        let posted = read
            .topic
            .as_ref()
            .filter(|(_, at)| words.is_none_or(|(_, said)| at > said));
        let program = read.program.as_deref().unwrap_or("agent");
        let state = if read.turn_open { "working" } else { IDLE };
        return [
            read.need.clone().map(|need| (need, false)),
            posted.map(|(post, _)| (post.clone(), false)),
            words.map(|(words, _)| (words.clone(), false)),
            Some((format!("{program} {state}"), read.words_unread)),
        ]
        .into_iter()
        .flatten()
        .collect();
    }
    let mut text = match (read.running, &read.program) {
        (true, Some(program)) => format!("running {program}"),
        (true, None) => "running".to_owned(),
        (false, _) => IDLE.to_owned(),
    };
    if let Some((word, ended)) = &read.last_cmd {
        text.push_str(" · ");
        text.push_str(word);
        match ended {
            Ended::Exit(code) => text.push_str(&format!(" exit {code}")),
            Ended::Signal => text.push_str(" signal"),
            Ended::Unknown => {}
        }
    }
    vec![(text, false)]
}

/// THE TRIGGERS a description is re-derived at (the comprehension design §3.3): a turn close, a
/// need opening or clearing, an agent run opening or closing, a task's state moving, a command or
/// program run opening or closing, a folder change, and a topic post. The agent's words are read
/// again only when one moves, never at every word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Trigger {
    turn: Option<(Node, u64)>,
    needs: u64,
    agent: Option<Node>,
    run: Option<(Node, u64)>,
    last_run: Option<(Node, u64)>,
    task: Option<(&'static str, bool)>,
    cwd: u64,
    topic: Option<u64>,
}

/// WHAT T0 LAST TOOK OF AN AGENT'S WORDS: the triggers they were taken at, the words, whether
/// they were older than that trigger (the transcript's read lags a turn's close), so that the
/// next move of the excerpts takes them once more, and the excerpts as they were then.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Said {
    trigger: Option<Trigger>,
    words: Option<(String, u64)>,
    owed: bool,
    excerpts: u64,
}

/// What one pass read of a session: the log's position, the excerpts ([`excerpts_seen`], their
/// prompts among them while the open agent run's title wants its first), the topic post's time,
/// the task, and the folder and whether it is yet to be read. A pass that reads the same does
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    ep: u32,
    rev: u64,
    seq: u64,
    excerpts: u64,
    topic: Option<u64>,
    task: u64,
    cwd: u64,
    cwd_unread: bool,
}

/// WHAT T0 KEEPS OF ONE SESSION BETWEEN ITS PASSES, in the session's timeline: what the last
/// finished pass read, the agent's words as of the last trigger, and its own bucket of records
/// ([`crate::session_log::observed::PROGRAM_BURST`] at once, then one each
/// [`crate::session_log::observed::PROGRAM_REFILL_MS`]), never the mark limiter's: aterm's slot
/// there is the overflow bucket other authors drain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Held {
    seen: Option<Seen>,
    said: Said,
    bucket: MarkBucket,
}

/// Feed `text` (or its absence) into `h`, its length first, so two texts never run together.
fn feed(h: &mut Fnv1a64, text: Option<&str>) {
    match text {
        Some(text) => {
            h.write(&(text.len() as u64).to_le_bytes());
            h.write(text.as_bytes());
        }
        None => h.write(&u64::MAX.to_le_bytes()),
    }
}

/// The excerpts of the agent in front, as the survey reads them.
fn excerpts_of(tl: &SessionTimeline) -> Option<&Excerpts> {
    tl.claude_excerpts()
        .map(|e| &e.excerpts)
        .or_else(|| tl.codex_survey().map(|s| &s.facts.excerpts))
}

/// WHAT A PASS READS OF `tl`'S EXCERPTS: their three words and their row's time; whether they
/// are yet to be read since an update ([`SessionTimeline::words_unread`]), so the first read
/// moves it even where it found none; and, only while the open agent run's title wants its
/// first prompt ([`wants_prompt`]), the key of each prompt that could be it (every one that is
/// no bash-mode command), in order, so any new one moves it; with a task, once a prompt is kept,
/// or for a command the person ran, the prompts move nothing. The excerpts' writers ring the lane
/// only where this moved, so a re-read that found what it held runs no pass.
pub(crate) fn excerpts_seen(tl: &SessionTimeline) -> u64 {
    let mut excerpts = Fnv1a64::new();
    let e = excerpts_of(tl);
    if let Some(e) = e {
        feed(&mut excerpts, e.said.as_deref());
        feed(&mut excerpts, e.said_end.as_deref());
        feed(&mut excerpts, e.doing.as_deref());
        excerpts.write(&e.said_at_ms.unwrap_or(i64::MIN).to_le_bytes());
    }
    excerpts.write(&[u8::from(tl.words_unread())]);
    if let Some(e) = e.filter(|_| wants_prompt(tl).is_some()) {
        for p in e.prompts.iter().filter(|p| !p.shell) {
            excerpts.write(&p.key.to_le_bytes());
        }
    }
    excerpts.finish()
}

/// What `tl` holds now, with the topic post's time `topic`.
fn seen(tl: &SessionTimeline, topic: Option<u64>) -> Seen {
    let log = tl.slog().log();
    let st = log.state();
    let mut task = Fnv1a64::new();
    if let Some(t) = tl.task() {
        feed(&mut task, t.line.as_deref());
        feed(&mut task, Some(t.state.as_str()));
        task.write(&[u8::from(t.conclusion().is_some())]);
    }
    let mut cwd = Fnv1a64::new();
    feed(&mut cwd, tl.cwd_text());
    Seen {
        ep: st.ep,
        rev: st.rev,
        seq: log.last_seq(),
        excerpts: excerpts_seen(tl),
        topic,
        task: task.finish(),
        cwd: cwd.finish(),
        cwd_unread: tl.cwd_unread(),
    }
}

/// The open agent run of `tl`'s session where its title wants a first prompt: the session has no
/// task (its first prompt is the task, whose line titles it) and the run has kept none, in this
/// process or before an update ([`crate::session_log::SessionLog::kept_first_prompt`]).
fn wants_prompt(tl: &SessionTimeline) -> Option<Node> {
    if tl.task().is_some() {
        return None;
    }
    let log = tl.slog().log();
    log.state()
        .desc
        .open_agent()
        .filter(|a| log.is_open(*a) && !tl.slog().kept_first_prompt(*a))
}

/// Whether aterm reads the words of the agent `word` names at all: the footer's resolver reads
/// those of the agents it draws a footer for ([`crate::claude_footer::drawn_for`]), the rollout
/// tap codex's. Another agent's words are never read, so none is unread.
fn words_read_for(word: &str) -> bool {
    use aterm_phase::Program;
    let tapped = aterm_phase::program_of(word).is_some_and(|program| match program {
        Program::Codex => true,
        Program::Claude
        | Program::Agy
        | Program::Muse
        | Program::Grok
        | Program::Kimi
        | Program::Generic => false,
    });
    tapped || crate::claude_footer::drawn_for(Some(word))
}

/// A run's word: an agent's `agent=` (else its `prog=`), a program's `prog=`, a command's
/// `detail=`, which the log writes only once the kernel confirms that program ran.
fn word_of(run: &OpenNode) -> Option<&str> {
    match run.kind {
        Kind::Agent => run.f.str(Key::Agent).or_else(|| run.f.str(Key::Prog)),
        Kind::Prog => run.f.str(Key::Prog),
        Kind::Cmd => run.f.str(Key::Detail),
        _ => None,
    }
}

/// WHERE A PASS GETS THE OPEN AGENT RUN'S FIRST PROMPT, for a session with no task.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Want {
    /// None is wanted: the session has a task, or no agent run is open, or nothing holds one.
    #[default]
    None,
    /// The prompt the run keeps, or T0's own `as=prompt` title of it (which the update carried
    /// where the run's kept prompt was lost).
    Kept(String),
    /// The excerpts' prompts, newest first, at least one of them no bash-mode command: the
    /// oldest the harness did not type that is not a bash-mode command is the run's first, read
    /// once the harness's keys are ([`pass_session`]).
    Pick(Vec<Prompt>),
}

/// WHAT A PASS TOOK of one session under its timeline lock: the copy [`derive`] reads, less what
/// is worked out after the lock drops (the folder's abbreviation, the agent's words, the topic's
/// words, the first prompt), and what decides those.
struct Taken {
    read: Read,
    seen: Seen,
    trigger: Trigger,
    /// When the newest trigger happened (wall ms): words older than it are owed.
    trigger_at: u64,
    /// Whether the agent's words are its closing ones: no turn open, or its task concluded.
    finished: bool,
    excerpts: Option<Excerpts>,
    words_read: WordsRead,
    cwd: Option<String>,
    said: Said,
    want: Want,
}

/// THE COPY of `tl`'s session a pass derives from, with the topic post's time `topic`: `None`
/// where its session is closed, or where nothing it reads moved since the last finished pass.
fn take(tl: &SessionTimeline, topic: Option<u64>) -> Option<Taken> {
    let log = tl.slog().log();
    let st = log.state();
    if st.closed.is_some() {
        return None;
    }
    let seen = seen(tl, topic);
    let held = tl.described();
    if held.seen == Some(seen) {
        return None;
    }
    let agent = st.desc.open_agent().filter(|a| log.is_open(*a));
    let run = st.run();
    let program = match agent {
        Some(a) => run
            .filter(|r| r.node == a)
            .and_then(word_of)
            .or_else(|| tl.slog().agent_word(a)),
        None => run.and_then(word_of),
    };
    let words_unread = agent.is_some() && tl.words_unread() && program.is_some_and(words_read_for);
    let need = agent
        .and(st.top_need())
        .and_then(|n| aterm_slog::need_words(n.word, n.detail.as_deref()))
        .map(|(words, _)| words.as_str().to_owned());
    let last_cmd = st.last[0]
        .as_ref()
        .filter(|c| c.kind == Kind::Cmd)
        .and_then(|c| {
            let word = c.f.str(Key::Detail)?;
            let ended = match (c.out, c.f.i(Key::Code)) {
                ("signal", _) => Ended::Signal,
                (_, Some(code)) => Ended::Exit(code),
                (_, None) => Ended::Unknown,
            };
            Some((word.to_owned(), ended))
        });
    let task = tl.task();
    let mut needs = Fnv1a64::new();
    for n in st.needs.iter().flatten() {
        feed(&mut needs, Some(n.word));
        needs.write(&n.since_seq.to_le_bytes());
    }
    let trigger = Trigger {
        turn: st.last[1].as_ref().map(|c| (c.node, c.t)),
        needs: needs.finish(),
        agent,
        run: run.map(|r| (r.node, r.t)),
        last_run: st.last[0].as_ref().map(|c| (c.node, c.t)),
        task: task.map(|t| (t.state.as_str(), t.conclusion().is_some())),
        cwd: seen.cwd,
        topic,
    };
    let trigger_at = [
        st.last[1].as_ref().map(|c| c.t),
        st.needs.iter().flatten().map(|n| n.t).max(),
        run.map(|r| r.t),
        st.last[0].as_ref().map(|c| c.t),
        topic,
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(0);
    let turn_open = st.turn().is_some();
    let concluded = task.is_some_and(|t| t.conclusion().is_some());
    // The first source that holds the run's first prompt: the run's own; T0's title of it; none
    // where the run kept one that no claim could hold (a credential, a protocol line, nothing
    // once cleaned), which an update carried as a fact alone, so the title the update carried
    // stands; else the excerpts' prompts to pick from, where one is no bash-mode command.
    let want = agent
        .filter(|_| task.is_none())
        .and_then(|run| {
            let kept = tl.slog().first_prompt(run).map(str::to_owned).or_else(|| {
                st.desc
                    .claim_of(run, TITLE, BY, SRC, None)
                    .filter(|c| c.as_name.as_deref() == Some(Rung::Prompt.word()))
                    .map(|c| c.value.clone())
            });
            if let Some(kept) = kept {
                return Some(Want::Kept(kept));
            }
            if tl.slog().kept_first_prompt(run) {
                return None;
            }
            excerpts_of(tl)
                .filter(|e| e.prompts.iter().any(|p| !p.shell))
                .map(|e| Want::Pick(e.prompts.clone()))
        })
        .unwrap_or_default();
    let excerpts = agent.and(excerpts_of(tl)).map(|e| Excerpts {
        prompts: Vec::new(),
        said: e.said.clone(),
        said_end: e.said_end.clone(),
        said_at_ms: e.said_at_ms,
        doing: e.doing.clone(),
    });
    Some(Taken {
        read: Read {
            agent,
            program: program.map(str::to_owned),
            running: run.is_some(),
            turn_open,
            need,
            last_cmd,
            task_line: task.and_then(|t| t.line.clone()),
            folder_unread: tl.cwd_unread(),
            words_unread,
            ..Read::default()
        },
        seen,
        trigger,
        trigger_at,
        finished: !turn_open || concluded,
        excerpts,
        words_read: tl.words_read(),
        cwd: tl.cwd_text().map(str::to_owned),
        said: held.said.clone(),
        want,
    })
}

impl Taken {
    /// THE READ, worked out with no lock held: the folder home-abbreviated, the topic post's
    /// words, the agent's words, taken afresh at a trigger, and once more at the next move of
    /// the excerpts after a trigger they were older than (a move of their prompts alone, which
    /// left the words as they were, leaves them owed), else as they were taken last, and the
    /// open agent run's first prompt: the one it keeps, else the oldest of the excerpts' prompts
    /// that is not a bash-mode command and whose key is not in `harness` (the prompts aterm's
    /// harness typed, [`crate::survey::harness_keys`]).
    fn settle(self, topic: Option<&TopicPost>, harness: &[u64]) -> (Read, Said) {
        let fresh = || {
            self.excerpts
                .as_ref()
                .and_then(|e| crate::survey::excerpt_last_words(e, self.words_read, self.finished))
        };
        let said = if self.read.agent.is_none() {
            Said {
                trigger: Some(self.trigger),
                ..Said::default()
            }
        } else if self.said.trigger != Some(self.trigger) {
            let words = fresh();
            let owed = words.as_ref().is_none_or(|(_, at)| *at < self.trigger_at);
            Said {
                trigger: Some(self.trigger),
                words,
                owed,
                excerpts: self.seen.excerpts,
            }
        } else if self.said.owed && self.said.excerpts != self.seen.excerpts {
            // What a pass reads of the excerpts holds their prompts too: a move of those alone
            // finds the words it took, which are still owed.
            let words = fresh();
            Said {
                owed: words == self.said.words,
                words,
                excerpts: self.seen.excerpts,
                ..self.said.clone()
            }
        } else {
            self.said.clone()
        };
        let prompt = match self.want {
            Want::None => None,
            Want::Kept(prompt) => Some(prompt),
            Want::Pick(prompts) => prompts
                .into_iter()
                .rev()
                .find(|p| !p.shell && !harness.contains(&p.key))
                .map(|p| p.text),
        };
        let read = Read {
            folder: self.cwd.as_deref().map(crate::app_tabs::home_abbreviated),
            words: said.words.clone(),
            topic: topic.and_then(|t| crate::survey::topic_words(t).map(|w| (w, t.at_ms))),
            prompt,
            ..self.read
        };
        (read, said)
    }
}

/// One record T0 writes: a claim, or (`text: None`) the withdrawal of its own.
struct Write {
    on: Node,
    slot: &'static str,
    text: Option<String>,
    as_name: Option<&'static str>,
}

/// The line T0 claims of `slot` from `lines`, best first, as stored ([`observed_stored`]): its
/// subject, text and `as=`; `None` where T0's standing line stays and nothing is written.
///
/// A line that rests on an input this process has yet to read since an update ([`Line::unread`])
/// stops there wherever T0 holds a claim of `slot` on its subject that the update carried (at or
/// below `carried`, the adopted log's newest seq): that claim stands until the input is read. A
/// line the description rule refuses falls to the next, except where `keeps`, the first line is
/// refused for its form (only a credential, or nothing once cleaned: words that are current but
/// cannot be shown) and T0's line already stands on its subject: that line stays.
fn chosen(
    desc: &Described,
    slot: &'static str,
    lines: &[Line],
    keeps: bool,
    carried: u64,
) -> Option<(Node, String, Option<&'static str>)> {
    let held = |on: Node| desc.claim_of(on, slot, BY, SRC, None);
    for (i, line) in lines.iter().enumerate() {
        if line.unread && held(line.on).is_some_and(|c| c.seq <= carried) {
            return None;
        }
        match observed_stored(slot, &line.text, false, BY, SRC) {
            Ok(text) => return Some((line.on, text, line.rung.map(Rung::word))),
            Err(DescRefused::Credential | DescRefused::Claim(ClaimRefused::Empty))
                if keeps && i == 0 && held(line.on).is_some() =>
            {
                return None;
            }
            // Any other refusal (a protocol line: `Nothing left:`, `LANDED`, `NEEDS YOU:`,
            // `Could not finish:`) says nothing T0 may claim, and keeping the line before it
            // would keep a state that has passed: the next line, at worst `<word> idle|working`,
            // is written.
            Err(_) => {}
        }
    }
    None
}

/// WRITE T0'S LINES into `tl`'s log at `at`, under the timeline lock alone: for each slot, the
/// line [`chosen`] gives, where it differs (in text or `as=`) from T0's own claim on that subject
/// in the fold, so a carried claim the same inputs give is not written again after an update and
/// a line that did not move writes nothing; and first the withdrawal of T0's claim of that slot
/// on any other subject.
/// Through the one entry for claims aterm observes or derives
/// ([`crate::session_log::SessionLog::claim_observed`]: redacted always, no mark token spent).
/// One token of T0's own bucket pays for a pass's records; short of one, nothing is written and
/// the ms until the bucket earns one is returned. A closed session takes nothing. Returns how
/// many records it wrote.
pub(crate) fn write(tl: &mut SessionTimeline, lines: &Lines, at: Stamp) -> Result<usize, u64> {
    let st = tl.slog().log().state();
    if st.closed.is_some() {
        return Ok(0);
    }
    let desc = &st.desc;
    let carried = tl.carried_seq();
    let mut writes: Vec<Write> = Vec::with_capacity(4);
    for (slot, lines, keeps) in [
        (TITLE, &lines.title, false),
        (DESCRIPTION, &lines.description, true),
    ] {
        let Some((on, text, as_name)) = chosen(desc, slot, lines, keeps, carried) else {
            continue;
        };
        for sub in desc.subjects() {
            if sub.node != on && desc.claim_of(sub.node, slot, BY, SRC, None).is_some() {
                writes.push(Write {
                    on: sub.node,
                    slot,
                    text: None,
                    as_name: None,
                });
            }
        }
        let held = desc.claim_of(on, slot, BY, SRC, None);
        if held.is_none_or(|c| c.value != text || c.as_name.as_deref() != as_name) {
            writes.push(Write {
                on,
                slot,
                text: Some(text),
                as_name,
            });
        }
    }
    if writes.is_empty() {
        return Ok(0);
    }
    let bucket = &mut tl.described_mut().bucket;
    if !bucket.spend(at.mono_ms, false) {
        return Err(bucket.next_token_at().saturating_sub(at.mono_ms));
    }
    for w in &writes {
        let claim = ObservedClaim {
            on: w.on,
            slot: w.slot,
            text: w.text.as_deref(),
            content: false,
            by: BY,
            src: SRC,
            conf: CONF,
            as_name: w.as_name,
        };
        // The log refuses a record only for a closed session, whose claims go with it.
        let _ = tl.slog_mut().claim_observed(&claim, at);
    }
    let _ = tl.slog_take_wake();
    Ok(writes.len())
}

/// ONE SESSION'S PASS: its topic post read with no lock held, its copy taken under its timeline
/// lock alone (`try_lock`: a contended lock leaves it owed), the keys of the prompts aterm's
/// harness typed read under the session's turn ledger's lock alone where a first prompt is to be
/// picked (`try_lock` too), its lines derived with no lock held, and written under the timeline
/// lock again, after the open agent run keeps the first prompt its title read
/// ([`crate::session_log::SessionLog::keep_first_prompt`]), so what the pass records it read says
/// so. `Some(ms)` where it is owed again in `ms` (0: as soon as a pass may run), `None` where
/// nothing is owed.
fn pass_session(ctx: &crate::SessionCtx) -> Option<u64> {
    let topic = ctx.fabric.last_topic_post();
    let topic_at = topic.as_ref().map(|t| t.at_ms);
    let taken = {
        let tl = match ctx.timeline.try_lock() {
            Ok(tl) => tl,
            Err(TryLockError::WouldBlock) => return Some(0),
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
        };
        take(&tl, topic_at)?
    };
    let read_at = taken.seen;
    let harness = match &taken.want {
        Want::Pick(_) => match ctx.turns.try_lock() {
            Ok(turns) => crate::survey::harness_keys(&turns),
            Err(TryLockError::WouldBlock) => return Some(0),
            Err(TryLockError::Poisoned(p)) => crate::survey::harness_keys(&p.into_inner()),
        },
        Want::None | Want::Kept(_) => Vec::new(),
    };
    let (read, said) = taken.settle(topic.as_ref(), &harness);
    let lines = derive(&read);
    let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let moved = seen(&tl, topic_at) != read_at;
    if let (Some(run), Some(prompt)) = (read.agent, read.prompt.as_deref()) {
        tl.slog_mut().keep_first_prompt(run, prompt);
    }
    let at = tl.slog().now();
    let wrote = write(&mut tl, &lines, at);
    let after = seen(&tl, topic_at);
    let held = tl.described_mut();
    held.said = said;
    match wrote {
        // What another writer changed since the copy rang the lane, and is read at the next
        // pass.
        Ok(_) if !moved => held.seen = Some(after),
        Ok(_) => {}
        Err(ms) => return Some(ms),
    }
    None
}

thread_local! {
    /// Whether this thread is in a pass: its own records ring no other ([`ring`]).
    static PASSING: Cell<bool> = const { Cell::new(false) };
}

/// A pass in progress on this thread, until dropped.
struct Passing;

impl Passing {
    fn enter() -> Passing {
        PASSING.with(|p| p.set(true));
        Passing
    }
}

impl Drop for Passing {
    fn drop(&mut self) {
        PASSING.with(|p| p.set(false));
    }
}

/// ONE PASS over `store`'s sessions: the handles are taken and the store's lock dropped before
/// any session is read. `Some(ms)` where a session is owed again in `ms` (its timeline was
/// contended, or its bucket short), the soonest of them; `None` where nothing is owed.
pub(crate) fn pass(store: &crate::session_store::Store) -> Option<u64> {
    let _passing = Passing::enter();
    let handles = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
    handles.iter().filter_map(|h| pass_session(&h.ctx)).min()
}

/// The lane: whether a pass is owed (set by a ring, taken when a pass starts), the thread once
/// it runs, and the callback a session log rings on every append where no journal lane holds it.
struct Lane {
    rung: AtomicBool,
    thread: OnceLock<std::thread::Thread>,
    notify: aterm_slog::Notify,
}

/// This process's lane, once the app's startup published it.
static LANE: OnceLock<Lane> = OnceLock::new();

/// PUBLISH THE LANE, at the app's startup before its first session: from here a ring is kept,
/// and a log made where this launch keeps no journal rings it ([`notify`]). Owed from the start,
/// so the first pass runs once the thread does.
pub(crate) fn publish() {
    let _ = LANE.get_or_init(|| Lane {
        rung: AtomicBool::new(true),
        thread: OnceLock::new(),
        notify: aterm_slog::Notify::new(ring),
    });
}

/// RING THE LANE: a session's inputs may have moved. A leaf that takes no lock and allocates
/// nothing, so it is rung from inside an append: an atomic swap, and an unpark only where it
/// found the lane unrung. The flag stays set until a pass starts, so every later ring before
/// then (a held pass's) costs the swap alone. Nothing where the lane was never published (every
/// test), and nothing from a pass's own records.
pub(crate) fn ring() {
    let Some(lane) = LANE.get() else {
        return;
    };
    if PASSING.with(Cell::get) {
        return;
    }
    if !lane.rung.swap(true, Ordering::AcqRel)
        && let Some(thread) = lane.thread.get()
    {
        thread.unpark();
    }
}

/// The callback a session log installs where this launch keeps no journal
/// ([`crate::session_log_journal::notify`]): it rings this lane. `None` until it is published.
pub(crate) fn notify() -> Option<aterm_slog::Notify> {
    LANE.get().map(|lane| lane.notify.clone())
}

/// START THE THREAD `aterm-describe` over `store`, once, after [`publish`]: utility QoS.
pub(crate) fn start(store: crate::session_store::Store) {
    let Some(lane) = LANE.get() else {
        return;
    };
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned =
        crate::qos::spawn_lane_thread("aterm-describe", crate::qos::Role::Background, move || {
            run(lane, &store)
        });
    if let Err(error) = spawned {
        aterm_log::warn!("describe: the default writer did not start ({error})");
    }
}

/// The thread: asleep with no timer while nothing is owed; rung, a pass no sooner than
/// [`PASS_GAP`] after the last one started; a session left owed arms the one deadline. The ring
/// is read, not taken, while the pass is held, and taken as the pass starts.
fn run(lane: &'static Lane, store: &crate::session_store::Store) {
    let _ = lane.thread.set(std::thread::current());
    let mut last: Option<Instant> = None;
    let mut due: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if lane.rung.load(Ordering::Acquire) {
            let earliest = last.map_or(now, |l| (l + PASS_GAP).max(now));
            due = Some(due.map_or(earliest, |d| d.min(earliest)));
        }
        let Some(at) = due else {
            // The flag was unset (a set one arms `due`); a ring since leaves the thread
            // notified, so the park returns at once.
            std::thread::park();
            continue;
        };
        if at > now {
            std::thread::park_timeout(at - now);
            continue;
        }
        // Taken before any session is read: a ring from here on finds the flag unset, wakes the
        // thread and is owed to the next pass.
        let _ = lane.rung.swap(false, Ordering::AcqRel);
        due = None;
        last = Some(now);
        if let Some(ms) = pass(store) {
            due = Some((now + PASS_GAP).max(Instant::now() + Duration::from_millis(ms)));
        }
    }
}

#[cfg(test)]
#[path = "describe_tests.rs"]
mod tests;
