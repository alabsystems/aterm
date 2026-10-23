// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WORKER'S START: `spawn agent=<claude|gemini|agy|muse>` (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.1), one mechanism for every harness (`aterm_agent::harness::worker`'s table).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::harness::upgrade::Dialect;
use aterm_agent::supervise::Autopilot;
use aterm_core::terminal::Terminal;
use aterm_session::SessionId;

use crate::SessionCtx;
use crate::session_store::{SessionState, Store};
use crate::task_record::{TaskMark, TaskState};

/// The longest task a worker is given: 64 KiB, raw, the client's cap too.
pub(crate) const TASK_MAX: usize = aterm_types::control_verbs::MAX_TASK_BYTES;

/// How long a worker's shell has to draw its first prompt before its task is `stalled why=shell`.
pub(crate) const FIRST_PROMPT_WAIT: Duration = Duration::from_secs(20);

/// The child deadline for `fish --version` ([`fish_says`]); complete-output
/// capture also retains the helper's bounded drain/reap reserves.
const VERSION_WAIT: Duration = Duration::from_secs(5);

/// The reply to a worker's spawn of `agent` that carried no task frame.
fn no_frame(agent: &str) -> String {
    format!(
        "ERR spawn agent={agent}: the task follows the line as a frame (len=<n>, then n bytes); \
         `aterm ctl spawn agent={agent} -` reads it from stdin\n"
    )
}

thread_local! {
    /// The task frame the serve loop read for the request this thread is
    /// serving ([`CarriedTask`]).
    static FRAME: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// The task a `spawn … len=<n>` frame carried, held for the ONE dispatch that serves its line (`control::serve_request_line`) and cleared when that dispatch ends, so no other request on the thread ever sees it.
pub(crate) struct CarriedTask(());

impl CarriedTask {
    /// Carry `task` (none for every other request) until the guard drops.
    pub(crate) fn enter(task: Option<Vec<u8>>) -> Self {
        FRAME.with(|frame| *frame.borrow_mut() = task);
        Self(())
    }
}

impl Drop for CarriedTask {
    fn drop(&mut self) {
        FRAME.with(|frame| frame.borrow_mut().take());
    }
}

/// The task frame the request being served carried, taken: the spawn handler reads it once ([`TaskOrder::of_frame`]).
pub(crate) fn take_frame() -> Option<Vec<u8>> {
    FRAME.with(|frame| frame.borrow_mut().take())
}

/// The bytes `len=<v>` announces, or the reply to a length the server will not read.
pub(crate) fn frame_len(v: &str, agent: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(n) if n <= TASK_MAX => Ok(n),
        _ if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => Err(too_large(v, agent)),
        _ => Err(crate::control::control_media::SPAWN_USAGE.to_string()),
    }
}

/// The refusal of a task over [`TASK_MAX`].
fn too_large(len: &str, agent: &str) -> String {
    format!("ERR spawn agent={agent}: the task is {len} bytes (max {TASK_MAX})\n")
}

/// The worker word a spawn line's `agent=` names, for a refusal made before the line is parsed whole ([`frame_len`]).
pub(crate) fn worker_word(agent: Option<&str>) -> &'static str {
    aterm_agent::harness::worker::WORKERS
        .iter()
        .find(|w| Some(w.word) == agent)
        .map_or("claude", |w| w.word)
}

/// What the control thread hands the main thread for a worker (`Wake::SpawnSession`'s `task`): checked, but not yet written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskOrder {
    /// The task as the frame carried it.
    pub(crate) task: Vec<u8>,
    /// `agent=`: the harness the worker runs, one with a row in `aterm_agent::harness::worker::WORKERS`.
    pub(crate) agent: aterm_phase::Program,
    /// `model=`: the worker's `--model`.
    pub(crate) model: Option<String>,
    /// `from=`, the session that starts the worker (the client's own, which the spawn's `@<sid>` aim need not be).
    pub(crate) caller: Option<SessionId>,
    /// The `cwd=` named, canonical.
    pub(crate) trusted_cwd: Option<String>,
    /// Whether the task file ends with the [`fact_line`] naming its starter: a client's spawn's does.
    pub(crate) fact_line: bool,
    /// The newborn's own Autopilot word (`spawn … autopilot=<land|cruise>`), written beside its trusted folder before its agent runs ([`WORKER_AUTOPILOT`] where the spawn names none).
    pub(crate) autopilot: Autopilot,
}

/// A WORKER'S AUTOPILOT where its spawn names none: Land (Autopilot Land).
pub(crate) const WORKER_AUTOPILOT: Autopilot = Autopilot::Land;

impl TaskOrder {
    /// This order under the spawn's `autopilot=` word (`None`: [`WORKER_AUTOPILOT`]).
    #[must_use]
    pub(crate) fn under(self, autopilot: Option<Autopilot>) -> Self {
        Self {
            autopilot: autopilot.unwrap_or(WORKER_AUTOPILOT),
            ..self
        }
    }

    /// The order a worker's spawn makes of its `frame` ([`take_frame`]): `Err` is the reply.
    pub(crate) fn of_frame(
        frame: Option<Vec<u8>>,
        agent: aterm_phase::Program,
        model: Option<String>,
        cwd: Option<&str>,
        caller: Option<SessionId>,
    ) -> Result<Self, String> {
        let word = agent.name();
        let Some(task) = frame else {
            return Err(no_frame(word));
        };
        if task.is_empty() {
            return Err(format!("ERR spawn agent={word}: the task is empty\n"));
        }
        if task.len() > TASK_MAX {
            return Err(too_large(&task.len().to_string(), word));
        }
        if task.contains(&0) {
            return Err(format!(
                "ERR spawn agent={word}: the task holds a NUL byte, which no argument can carry\n"
            ));
        }
        let trusted_cwd = cwd.and_then(crate::spawn_folder::canonical_folder);
        Ok(Self {
            task,
            agent,
            model,
            caller,
            trusted_cwd,
            fact_line: true,
            autopilot: WORKER_AUTOPILOT,
        })
    }
}

/// The ONE LINE OF FACT a worker's task gains (design §2.1; never an order — the primer's rule).
pub(crate) fn fact_line(from: &str, autopilot: Autopilot, agent: &str) -> String {
    let end = match autopilot {
        Autopilot::Cruise => format!(
            "under Autopilot Cruise: no report ends the work, and `{from}` reads its progress \
             from this session"
        ),
        Autopilot::Land | Autopilot::Off => format!(
            "under Autopilot Land: the work ends once all of it is on the remote main branch \
             and a reply defends that in a few lines starting `Nothing left:`, and that report \
             is returned verbatim to `{from}` as your result"
        ),
    };
    format!(
        "(Started by `{from}` with `spawn agent={agent}`, {end}. A line starting `NEEDS YOU:` \
         reaches `{from}` and the person while the work goes on.)"
    )
}

/// What a worker's task file holds: the task as given, its last line ended, then.
pub(crate) fn task_text(
    task: &[u8],
    from: Option<&str>,
    autopilot: Autopilot,
    agent: aterm_phase::Program,
) -> Vec<u8> {
    let blank_lines =
        aterm_agent::harness::worker::worker_cli(agent).is_none_or(|w| w.blank_lines());
    let mut text = if blank_lines {
        task.to_vec()
    } else {
        without_blank_lines(task)
    };
    if !text.is_empty() && !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    if let Some(from) = from {
        if blank_lines {
            text.push(b'\n');
        }
        text.extend_from_slice(fact_line(from, autopilot, agent.name()).as_bytes());
        text.push(b'\n');
    }
    text
}

/// `task` with every line that draws as a blank row left out (one of white
/// space alone, Unicode's as agy's reader reads a blank row — `U+00A0`,
/// `U+3000`, `U+2000`..`U+200A` and the rest — or of characters that draw
/// nothing: controls and the zero-width ones, [`draws_nothing`]), each other
/// line as given.
fn without_blank_lines(task: &[u8]) -> Vec<u8> {
    task.split_inclusive(|&b| b == b'\n')
        .filter(|line| !String::from_utf8_lossy(line).chars().all(draws_nothing))
        .flatten()
        .copied()
        .collect()
}

/// Whether `c` draws nothing a reader of the row could tell from a blank
/// cell: white space (Unicode's `White_Space`, what `aterm_phase`'s readers
/// trim), a control character, or a zero-width format character (the soft
/// hyphen, the zero-width space, joiners and marks `U+200B`..`U+200F`, the
/// word joiner and invisible operators `U+2060`..`U+2064`, the byte-order
/// mark).
fn draws_nothing(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
        )
}

/// A worker's task file, ready to be written once the newborn's sid is known (`spawn::spawn_session`, on the spawner thread a tab's shell starts on — `tab_spawn`).
#[derive(Clone)]
pub(crate) struct TaskSeed {
    text: Arc<[u8]>,
    trust_text: Option<Arc<[u8]>>,
    /// The write and explicit discard share one decision across clones.
    written: Arc<Mutex<TaskFileState>>,
}

#[derive(Default)]
struct TaskFileState {
    discarded: bool,
    path: Option<PathBuf>,
    trust_path: Option<PathBuf>,
}

impl TaskSeed {
    /// The seed of a file holding `text` ([`task_text`]).
    pub(crate) fn new(text: Vec<u8>) -> Self {
        Self {
            text: text.into(),
            trust_text: None,
            written: Arc::default(),
        }
    }

    /// Prepare the worker's optional private trust sidecar in the same spawner write and cancellation lifetime as its task, before the session can be registered and closed.
    pub(crate) fn with_trust_file(mut self, folder: &str) -> Self {
        self.trust_text = Some(
            aterm_agent::harness::worker::trusted_folders_text(folder)
                .into_bytes()
                .into(),
        );
        self
    }

    pub(crate) fn trust_file(&self) -> Option<PathBuf> {
        self.written
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .trust_path
            .clone()
    }

    /// Remove the task and optional trust file this seed wrote: its worker did not start, so nothing will read them.
    pub(crate) fn discard(&self) {
        let written = {
            let mut state = self.written.lock().unwrap_or_else(|p| p.into_inner());
            state.discarded = true;
            [state.path.take(), state.trust_path.take()]
        };
        for path in written.into_iter().flatten() {
            if let Err(e) = std::fs::remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                aterm_log::warn!("task file {}: not removed: {e}", path.display());
            }
        }
    }

    /// Write the task file of session `sid`, `<state>/tasks/<sid>-1.txt` (a new record is rev 1).
    pub(crate) fn write_for(&self, sid: &SessionId) -> std::io::Result<PathBuf> {
        let dir = crate::task_record::tasks_dir().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no state directory for the task file",
            )
        })?;
        self.write_in(&dir, sid)
    }

    /// [`Self::write_for`] in the tasks directory `dir`.
    pub(crate) fn write_in(&self, dir: &Path, sid: &SessionId) -> std::io::Result<PathBuf> {
        self.write_in_using(dir, sid, |pinned, name, text| {
            pinned.write_new_private(name, text)
        })
    }

    /// The exclusive file write and its ownership handover. The filesystem
    /// operation runs without the state lock, so discard never waits on it.
    fn write_in_using(
        &self,
        dir: &Path,
        sid: &SessionId,
        write: impl FnOnce(
            &crate::pinned_dir::PinnedDir,
            &std::ffi::OsStr,
            &[u8],
        ) -> std::io::Result<crate::pinned_dir::PinnedFile>,
    ) -> std::io::Result<PathBuf> {
        let cancelled = || {
            std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "the worker spawn was cancelled",
            )
        };
        if self
            .written
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .discarded
        {
            return Err(cancelled());
        }
        crate::control_auth::ensure_private_dir(dir)?;
        let name = task_file_name(sid);
        let name = std::ffi::OsStr::new(&name);
        let pinned = crate::pinned_dir::PinnedDir::open_resolved(dir)?;
        let file = write(&pinned, name, &self.text)?;
        let path = dir.join(name);
        let trust_name = trust_file_name(sid);
        let trust = self.trust_text.as_ref().and_then(|text| {
            pinned
                .write_new_private(std::ffi::OsStr::new(&trust_name), text)
                .inspect_err(|e| {
                    aterm_log::warn!(
                        "worker {sid}: its trusted-folders file was not written ({e}): its \
                         folder-trust dialog is the person's"
                    );
                })
                .ok()
        });
        let mut state = self.written.lock().unwrap_or_else(|p| p.into_inner());
        if state.discarded {
            drop(state);
            let task_removed = file.remove_exact();
            let trust_removed = trust.map(|file| file.remove_exact()).transpose();
            task_removed?;
            trust_removed?;
            return Err(cancelled());
        }
        state.path = Some(path.clone());
        state.trust_path = trust.map(|_| dir.join(trust_name));
        Ok(path)
    }
}

/// The name of session `sid`'s task file in the tasks directory: a new
/// record is rev 1.
fn task_file_name(sid: &SessionId) -> String {
    format!("{}-1.txt", sid.as_str())
}

/// The name of a Gemini CLI worker `sid`'s own trusted-folders file in the
/// tasks directory ([`TaskSeed::with_trust_file`]).
fn trust_file_name(sid: &SessionId) -> String {
    format!("{}-1.trust.json", sid.as_str())
}

/// Whether `name` is one of session `sid`'s result files in the tasks
/// directory: `<sid>-<rev>.result.md` or its sidecar `<sid>-<rev>.result.src`
/// (`aterm_agent::harness::result::snapshot`, one pair per published done).
fn is_result_file_of(name: &str, sid: &SessionId) -> bool {
    let Some(stem) = name
        .strip_suffix(".result.md")
        .or_else(|| name.strip_suffix(".result.src"))
    else {
        return false;
    };
    stem.strip_prefix(sid.as_str())
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|rev| !rev.is_empty() && rev.bytes().all(|b| b.is_ascii_digit()))
}

/// The most names one retire reads of the tasks directory: far past what
/// its own sweeps leave there (a task file per live worker, a result pair
/// per done it published).
const TASKS_DIR_SCAN: usize = 65_536;

/// Remove session `sid`'s task files once the session is gone (`App::retire_session_registration`, the tail of every close; a quit's [`retire_task_files_at_quit`]).
pub(crate) fn retire_task_file(sid: &SessionId) {
    if let Some(dir) = crate::task_record::tasks_dir() {
        retire_task_file_in(&dir, sid);
    }
}

/// Remove the task files ([`retire_task_file`]) of every session `store` still holds with a task record, at a QUIT (Cmd-Q, `invoke Quit`, `aterm ctl quit`, a headless SIGTERM).
pub(crate) fn retire_task_files_at_quit(store: &Store, parked: &[SessionId]) {
    if let Some(dir) = crate::task_record::tasks_dir() {
        retire_task_files_in(&dir, store, parked);
    }
}

/// [`retire_task_files_at_quit`] in the tasks directory `dir`.
fn retire_task_files_in(dir: &Path, store: &Store, parked: &[SessionId]) {
    let tasked: Vec<SessionId> = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot()
        .into_iter()
        .filter(|h| {
            !parked.contains(&h.sid)
                && h.ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .task()
                    .is_some()
        })
        .map(|h| h.sid)
        .collect();
    retire_files_in(dir, &tasked);
}

/// [`retire_task_file`] in the tasks directory `dir`: none there, or no
/// directory, is no error.
fn retire_task_file_in(dir: &Path, sid: &SessionId) {
    retire_files_in(dir, std::slice::from_ref(sid));
}

/// Remove each of `sids`' task file and result files from the tasks
/// directory `dir`, the directory read once; none there, or no directory,
/// is no error.
fn retire_files_in(dir: &Path, sids: &[SessionId]) {
    if sids.is_empty() {
        return;
    }
    let pinned = match crate::pinned_dir::PinnedDir::open_resolved(dir) {
        Ok(pinned) => pinned,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            aterm_log::warn!("tasks directory {}: not read: {e}", dir.display());
            return;
        }
    };
    let remove = |name: &std::ffi::OsStr| {
        if let Err(e) = pinned.remove_file_if_exists(name) {
            aterm_log::warn!("task file {}: not removed: {e}", dir.join(name).display());
        }
    };
    for sid in sids {
        remove(std::ffi::OsStr::new(&task_file_name(sid)));
        remove(std::ffi::OsStr::new(&trust_file_name(sid)));
    }
    let names = match pinned.names_up_to(TASKS_DIR_SCAN) {
        Ok(names) => names,
        Err(e) => {
            aterm_log::warn!("tasks directory {}: not listed: {e}", dir.display());
            return;
        }
    };
    for name in &names {
        if name
            .to_str()
            .is_some_and(|n| sids.iter().any(|sid| is_result_file_of(n, sid)))
        {
            remove(name);
        }
    }
}

/// The dialect a worker's line is written in, for the shell `program` the spawn runs.
pub(crate) fn worker_dialect(program: &Path) -> Result<Dialect, String> {
    let name = program.to_string_lossy();
    Dialect::from_exe_name(&name).ok_or_else(|| {
        program
            .file_name()
            .map_or_else(|| name.to_string(), |n| n.to_string_lossy().into_owned())
    })
}

/// What a worker's launcher needs ([`launch_worker`]).
pub(crate) struct Launch {
    /// The worker's local id.
    pub(crate) local: u64,
    pub(crate) term: Arc<Mutex<Terminal>>,
    pub(crate) ctx: Arc<SessionCtx>,
    pub(crate) store: Store,
    /// The main-thread endpoint for the final launch admission, if one exists.
    pub(crate) proxy: Option<aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
    /// The person's count when the worker was registered, before its reply.
    pub(crate) human: u64,
    /// The worker's shell and the dialect its line is written in.
    pub(crate) shell: PathBuf,
    pub(crate) dialect: Dialect,
    /// `agent=`: the harness the line starts.
    pub(crate) agent: aterm_phase::Program,
    /// `model=`.
    pub(crate) model: Option<String>,
    /// The one folder the worker's starter trusted for it (the spawn's canonical `cwd=`).
    pub(crate) trusted_cwd: Option<String>,
    /// The optional sidecar already written with the task before the session was registered.
    pub(crate) trust_file: Option<PathBuf>,
    /// The starter's session, whose agent of the worker's kind the line follows ([`worker_starter_of`]).
    pub(crate) caller: Option<Arc<SessionCtx>>,
}

/// Start the worker's launcher.
pub(crate) fn launch_worker(work: Launch) {
    let (ctx, local) = (Arc::clone(&work.ctx), work.local);
    let started = std::thread::Builder::new()
        .name(format!("aterm-worker-{local}"))
        .spawn(move || run_worker_launch(&work));
    if started.is_err() {
        stall_task(&ctx, local, "its launcher thread could not start");
    }
}

/// The launcher's body ([`launch_worker`]).
fn run_worker_launch(work: &Launch) {
    if work.dialect == Dialect::Fish && !fish_answer(&work.shell) {
        stall(
            work,
            "its fish is older than 3.4 (or did not say its version)",
        );
        return;
    }
    let prompt = match await_first_prompt(work) {
        Ok(prompt) => prompt,
        Err(why) => {
            stall(work, why);
            return;
        }
    };
    let Some(cli) = aterm_agent::harness::worker::worker_cli(work.agent) else {
        stall(work, "its agent has no worker line");
        return;
    };
    let (caller, found) = worker_starter_of(work, cli);
    let here = trusted_here(
        cli.trust_flag.is_some(),
        work.trusted_cwd.as_deref(),
        || shell_cwd(&work.store, &work.ctx),
    );
    let session = cli.session_flag.and_then(|_| {
        aterm_agent::harness::worker::fresh_session_id()
            .inspect_err(|e| {
                aterm_log::warn!(
                    "worker session {}: no session id of its own ({e})",
                    work.local
                );
            })
            .ok()
    });
    let fresh = aterm_agent::harness::worker::Fresh {
        model: work.model.as_deref(),
        trusted_here: here,
        trust_file: work.trust_file.as_deref(),
        session: session.as_deref(),
    };
    let line = aterm_agent::harness::worker::line(cli, work.dialect, &caller, &fresh);
    match line {
        Ok(line) => {
            let Some(proxy) = work.proxy.as_ref() else {
                stall(work, "its main-thread launch endpoint is gone");
                return;
            };
            let written = match crate::control::control_media::call_main(proxy, |reply| {
                crate::Wake::WorkerLaunch {
                    sid: work.ctx.self_id.clone(),
                    nonce: work.ctx.nonce,
                    prompt,
                    line: line.clone(),
                    reply,
                }
            }) {
                Ok(Ok(written)) => written,
                Ok(Err(why)) => {
                    stall(work, &why);
                    return;
                }
                Err(why) => {
                    note_launch(
                        &work.ctx,
                        format!("outcome not observed ({why}); not retried"),
                    );
                    return;
                }
            };
            let note = format!(
                "typed ({} B, {written}): {} ({}{}{}{})",
                line.len() + 1,
                caller.exe.display(),
                found.words(cli.agent),
                if caller.env.is_empty() {
                    String::new()
                } else {
                    format!(", with its {} homes and login", cli.agent.product())
                },
                cli.trust_flag
                    .filter(|_| here)
                    .map(|flag| format!(", trusting its folder for this run ({flag})"))
                    .unwrap_or_default(),
                work.trust_file
                    .as_deref()
                    .map(|f| format!(", trusting its folder alone by {}", f.display()))
                    .unwrap_or_default(),
            );
            aterm_log::info!("worker session {}: launch line {note}", work.local);
            note_launch(&work.ctx, note);
        }
        Err(why) => stall(work, &format!("its launch line cannot be written: {why}")),
    }
}

/// Keep what the launcher did with the worker's line on its timeline
/// (`SessionTimeline::worker_launch`): the startup deadline's stall says it
/// (`harness_host::startup_mark_with`), so a worker that never started tells
/// a line typed and lost from one never typed.
fn note_launch(ctx: &SessionCtx, note: String) {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .note_worker_launch(note);
}

/// Wait for the worker's shell to draw its first prompt — the newest command
/// block one whose line editor reads (OSC 133 `B`: `blocks` says `entering`)
/// — for at most [`FIRST_PROMPT_WAIT`], woken by its output.
fn await_first_prompt(work: &Launch) -> Result<PromptFence, &'static str> {
    let deadline = Instant::now() + FIRST_PROMPT_WAIT;
    let sub = crate::session_watch::Subscription::single(&work.ctx.watchers);
    loop {
        let alive = work
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(work.local)
            .is_some_and(|h| !matches!(h.state, SessionState::Exited));
        if !alive {
            return Err("its shell ended before its first prompt");
        }
        let prompt = {
            let terminal = crate::term_lock(&work.term);
            let human = work.ctx.human_input.seq();
            if human != work.human {
                return Err("a person used its session before the launch");
            }
            prompt_fence(&terminal, human)
        };
        if let Some(prompt) = prompt {
            return Ok(prompt);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err("its shell drew no prompt within 20 s");
        }
        let _ = sub.wait(deadline - now);
    }
}

/// The shell prompt actually observed by the launcher, after its rc files.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromptFence {
    generation: crate::control::ScreenGen,
    human: u64,
}

/// Capture an input-ready prompt under the caller's terminal lock.
pub(crate) fn prompt_fence(terminal: &Terminal, human: u64) -> Option<PromptFence> {
    matches!(
        terminal.all_blocks().last().map(|b| b.state),
        Some(aterm_core::terminal::BlockState::EnteringCommand)
    )
    .then(|| PromptFence {
        generation: crate::control::screen_gen(terminal),
        human,
    })
}

/// Admit and type the worker's initial line ON THE MAIN THREAD.
pub(crate) fn type_launch_line(
    store: &Store,
    sid: &SessionId,
    nonce: aterm_session::LaunchNonce,
    prompt: PromptFence,
    line: &str,
) -> Result<&'static str, String> {
    use crate::control::{GuardedInput, InputFence};
    use crate::input::Delivery;
    let target = {
        let registry = store.read().unwrap_or_else(|p| p.into_inner());
        registry
            .by_sid(sid)
            .filter(|h| h.nonce == nonce && h.state != SessionState::Exited)
            .map(|h| (h.term.clone(), h.ctx.clone()))
    };
    let Some((term, ctx)) = target else {
        return Err("its live launch is gone".to_string());
    };
    if ctx.update_parked.is_parked() {
        return Err("its session is parked for an update".to_string());
    }
    if let Some(why) = crate::fabric::halt_refusal(ctx.as_ref(), "send") {
        return Err(why);
    }
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(0x0d);
    let result = crate::control::input_if_fenced_observed(
        &term,
        &ctx,
        crate::input::InputEvent::KeySequence(bytes),
        &InputFence {
            generation: Some(prompt.generation),
            human: Some(prompt.human),
            entering_command: true,
            ..InputFence::default()
        },
        // Inert: a fence that reads no row never waits, whichever thread is named.
        crate::ReadOn::EventLoop,
        None,
    );
    match result {
        GuardedInput::Changed | GuardedInput::Skipped => {
            Err("its startup prompt changed before the launch".to_string())
        }
        GuardedInput::Person => Err("a person used its session before the launch".to_string()),
        GuardedInput::Pressed(Delivery::Full) => Ok("written whole"),
        GuardedInput::Pressed(Delivery::PartialInDoubt { .. }) => Ok("written in part"),
        GuardedInput::Pressed(Delivery::BusyZero) => Err("its input sink is busy".to_string()),
        GuardedInput::Pressed(Delivery::Failed) => Err("its input write failed".to_string()),
    }
}

/// Mark the worker's task `stalled why=shell`, nothing typed, and say why in
/// the log and on its timeline ([`note_launch`]).
fn stall(work: &Launch, why: &str) {
    note_launch(&work.ctx, format!("never typed ({why})"));
    stall_task(&work.ctx, work.local, why);
}

/// [`stall`] of session `local` by its parts.
pub(crate) fn stall_task(ctx: &SessionCtx, local: u64, why: &str) {
    aterm_log::warn!("worker session {local}: {why}; its task is stalled and nothing was typed");
    let mut mark = TaskMark::new(TaskState::Stalled);
    mark.why = Some("shell");
    if crate::task_record::mark_session_task(ctx, local, &mark).is_err() {
        aterm_log::warn!(
            "worker session {local}: the stall was not marked (an update was committing)"
        );
    }
}

/// What a worker's line carries of the agent of its kind its starter runs (`found`).
pub(crate) fn line_starter(
    cli: &aterm_agent::harness::worker::WorkerCli,
    found: Option<aterm_agent::harness::worker::Starter>,
) -> aterm_agent::harness::worker::Starter {
    found.unwrap_or_else(|| cli.plain())
}

/// WHETHER THE WORKER'S LINE TRUSTS ITS FOLDER FOR THIS RUN (`flag`: the agent has such a flag, `aterm_agent::harness::worker::WorkerCli::trust_flag`, Muse Code's alone).
pub(crate) fn trusted_here(
    flag: bool,
    trusted: Option<&str>,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> bool {
    let Some(trusted) = trusted.filter(|_| flag) else {
        return false;
    };
    cwd().is_some_and(|cwd| cwd == Path::new(trusted))
}

/// The directory of the group in front of the worker's own terminal (its
/// shell, at its first prompt), as the kernel tells it.
fn shell_cwd(store: &Store, ctx: &SessionCtx) -> Option<PathBuf> {
    let pid = u32::try_from(starter_front(store, ctx)?).ok()?;
    aterm_sysprobe::process_cwd(pid)
}

/// Where a worker's line looked for its starter's agent of its kind ([`starter_group`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StarterGroup {
    /// The status sweep named the starter's program the worker's agent: its group.
    Named(i32),
    /// The sweep had not named the starter's program yet.
    Front(i32),
}

/// THE STARTER'S GROUP a worker's line follows: the one the status sweep named, where its reader is the worker's `agent`.
pub(crate) fn starter_group(
    reader: Option<aterm_phase::Program>,
    named: i32,
    front: impl FnOnce() -> Option<i32>,
    agent: aterm_phase::Program,
) -> Option<StarterGroup> {
    match reader {
        Some(reader) if reader == agent => Some(StarterGroup::Named(named)),
        Some(_) => None,
        None => front().filter(|g| *g > 0).map(StarterGroup::Front),
    }
}

/// What a worker's line found of its starter's agent, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    /// The status sweep's group ran it.
    Named,
    /// The starter's terminal's front group ran it; the sweep had not named
    /// it yet.
    Front,
    /// None: the plain word under the shell's own.
    Plain,
}

impl Found {
    /// The words the launch line's log names its program with, `agent` the
    /// worker's.
    fn words(self, agent: aterm_phase::Program) -> String {
        let product = agent.product();
        match self {
            Self::Named => format!("the starter's {product}"),
            Self::Front => format!(
                "the starter's {product}, read from its terminal: the status sweep had not named \
                 it yet"
            ),
            Self::Plain => format!("no {product} of a starter's: plain `{}`", agent.name()),
        }
    }
}

/// The program, homes and login a worker's line carries: the starter's own
/// agent of `cli`'s kind where the starter's session runs one
/// (`aterm_agent::harness::worker::starter`, of [`starter_group`]), else
/// `cli`'s plain word under the shell's own — with where it was found.
fn worker_starter_of(
    work: &Launch,
    cli: &aterm_agent::harness::worker::WorkerCli,
) -> (aterm_agent::harness::worker::Starter, Found) {
    let found = work.caller.as_deref().and_then(|ctx| {
        let (reader, named) = {
            let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let agent = timeline.agent();
            (agent.reader, agent.program_pgid)
        };
        let group = starter_group(reader, named, || starter_front(&work.store, ctx), cli.agent)?;
        let pgid = match group {
            StarterGroup::Named(g) | StarterGroup::Front(g) => g,
        };
        let pid = u32::try_from(pgid).ok().filter(|p| *p > 0)?;
        let starter =
            aterm_agent::harness::worker::starter(cli, pid, &aterm_types::dirs::home_dir()?)?;
        Some((
            starter,
            match group {
                StarterGroup::Named(_) => Found::Named,
                StarterGroup::Front(_) => Found::Front,
            },
        ))
    });
    let how = found.as_ref().map_or(Found::Plain, |(_, how)| *how);
    (line_starter(cli, found.map(|(s, _)| s)), how)
}

/// The group in front of the session's terminal now (`tcgetpgrp` on its PTY
/// master, read under the registry's read guard so the fd is the session's
/// own): the starter's, or the worker's own shell's; `None` for a session
/// that is gone, or where none can be read.
#[cfg(unix)]
fn starter_front(store: &Store, ctx: &SessionCtx) -> Option<i32> {
    let g = store.read().unwrap_or_else(|p| p.into_inner());
    let master = g
        .by_sid(&ctx.self_id)
        .filter(|h| !matches!(h.state, SessionState::Exited))?
        .master;
    // SAFETY: `tcgetpgrp` on the starter's own PTY master fd, which the registry holds open for the session's life (its guard is held).
    let pgrp = unsafe { libc::tcgetpgrp(master) };
    (pgrp > 0).then_some(pgrp)
}

/// No foreground group to read off unix: the sweep's naming alone.
#[cfg(not(unix))]
fn starter_front(_store: &Store, _ctx: &SessionCtx) -> Option<i32> {
    None
}

/// A fish program as a worker's spawn finds it: its path, and the file's
/// length and modification time there, so a fish upgraded in place is asked
/// again ([`fish_key`]).
type FishKey = (PathBuf, u64, Option<std::time::SystemTime>);

/// What a fish's `--version` came to ([`fish_answers`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FishSaid {
    /// Being asked.
    Asking,
    /// It named its version: whether that takes `$(…)`.
    Version(bool),
    /// It named none (it did not start, did not answer within
    /// [`VERSION_WAIT`], or said something else): not known to be old.
    Nothing,
}

/// What each fish said ([`FishKey`]).
fn fish_answers() -> &'static Mutex<std::collections::HashMap<FishKey, FishSaid>> {
    static ANSWERS: std::sync::OnceLock<Mutex<std::collections::HashMap<FishKey, FishSaid>>> =
        std::sync::OnceLock::new();
    ANSWERS.get_or_init(Mutex::default)
}

/// `shell`'s [`FishKey`]; `None` for a path that names no file (a bare
/// `fish`, which the shell's own search resolves): such a fish is asked by
/// every launcher and remembered by none.
fn fish_key(shell: &Path) -> Option<FishKey> {
    let meta = std::fs::metadata(shell)
        .ok()
        .filter(std::fs::Metadata::is_file)?;
    Some((shell.to_path_buf(), meta.len(), meta.modified().ok()))
}

fn remember_fish(key: FishKey, said: FishSaid) {
    fish_answers()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, said);
}

/// Whether the fish at `shell` is KNOWN to be older than 3.4.
pub(crate) fn fish_known_old(shell: &Path) -> bool {
    let Some(key) = fish_key(shell) else {
        return false;
    };
    {
        let mut answers = fish_answers().lock().unwrap_or_else(|p| p.into_inner());
        match answers.get(&key) {
            Some(FishSaid::Version(takes)) => return !takes,
            Some(FishSaid::Asking | FishSaid::Nothing) => return false,
            None => {
                answers.insert(key.clone(), FishSaid::Asking);
            }
        }
    }
    let asked = key.clone();
    let started = std::thread::Builder::new()
        .name("aterm-fish-version".to_string())
        .spawn(move || {
            let said = fish_says(&asked.0);
            remember_fish(asked, said);
        });
    if started.is_err() {
        fish_answers()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&key);
    }
    false
}

/// What the fish at `shell` answered [`fish_known_old`]'s question.
#[cfg(all(unix, test))]
pub(crate) fn fish_answered(shell: &Path) -> Option<Option<bool>> {
    let key = fish_key(shell)?;
    match *fish_answers()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key)?
    {
        FishSaid::Asking => None,
        FishSaid::Version(takes) => Some(Some(takes)),
        FishSaid::Nothing => Some(None),
    }
}

/// Whether the fish at `shell` takes `$(…)`, for a worker's launcher: the
/// version it named before ([`fish_known_old`]'s question, or another
/// launcher's), else asked now on the launcher's own thread and remembered.
fn fish_answer(shell: &Path) -> bool {
    let key = fish_key(shell);
    if let Some(key) = &key
        && let Some(FishSaid::Version(takes)) = fish_answers()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
    {
        return *takes;
    }
    let said = fish_says(shell);
    if let Some(key) = key {
        remember_fish(key, said);
    }
    said == FishSaid::Version(true)
}

/// What the fish at `shell` says of its version (`--version`), with a
/// child deadline of [`VERSION_WAIT`] and proven bounded stdout EOF through
/// the existing complete-output helper; otherwise [`FishSaid::Nothing`].
fn fish_says(shell: &Path) -> FishSaid {
    let mut cmd = aterm_uds::spawnfd::command(shell);
    cmd.arg("--version");
    let Ok(output) =
        aterm_uds::deadline::output_until_complete_stdout(&mut cmd, Instant::now() + VERSION_WAIT)
    else {
        return FishSaid::Nothing;
    };
    if output.stdout.len() > 4096 {
        return FishSaid::Nothing;
    }
    let Ok(said) = std::str::from_utf8(&output.stdout) else {
        return FishSaid::Nothing;
    };
    fish_version_takes_dollar_paren(said).map_or(FishSaid::Nothing, FishSaid::Version)
}

/// Whether `fish --version`'s answer (`fish, version 3.7.1`) names 3.4 or
/// later; `None` when it names no version.
fn fish_version_takes_dollar_paren(said: &str) -> Option<bool> {
    let version = said.split_whitespace().last()?;
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let (Some(Ok(major)), Some(Ok(minor))) = (
        parts.next().map(str::parse::<u32>),
        parts.next().map(str::parse::<u32>),
    ) else {
        return None;
    };
    Some((major, minor) >= (3, 4))
}

#[cfg(test)]
#[path = "task_launch_tests.rs"]
mod tests;
