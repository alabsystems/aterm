// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WORKER'S START: `spawn agent=claude` (`docs/DESIGN-sessions-as-subagents-2026-10-15.md`
//! §2.1). The newborn is the ordinary integrated login shell the spawn path
//! opens for every tab; four facts make it a worker:
//!
//! * its TASK — the bytes the request's frame carried, 1 byte to [`TASK_MAX`]
//!   — is written, with one line of fact, to a 0600 file under
//!   `<state>/tasks/` ([`TaskSeed::write_for`]) that the shell's
//!   `ATERM_TASK_FILE` names;
//! * its task record reads `working`, `from=` its starter, before the spawn
//!   answers (`App::spawn_session_aimed`), so a manager's `await task` has a
//!   record to wait on from the reply on;
//! * its own Autopilot word is written beside its trusted folder: `land`
//!   ([`WORKER_AUTOPILOT`]) unless the spawn says `autopilot=cruise`, so the
//!   window harness lands all of its work on the remote main branch and the
//!   record reads `done why=landed` at that landing (FOREVER LAND);
//! * at the shell's FIRST prompt (OSC 133 `B`: its line editor reads), ONE
//!   line is typed through the session's canonical egress ([`launch_worker`]):
//!   the window harness's relaunch builder's fresh start of Claude Code, its
//!   task the fixed operand `-- "$(command cat -- "$ATERM_TASK_FILE")"`
//!   (`aterm_agent::harness::relaunch::task_launch_line`). The task is argv —
//!   the agent's first prompt, neither a paste nor a peer's mail — so no byte of
//!   it is typed or parsed by the shell, and the shell stays under the agent:
//!   the harness's relaunch, live upgrade and memory restart need exactly that,
//!   and the pane outlives the agent.
//!
//! A shell that draws no prompt within [`FIRST_PROMPT_WAIT`], exits first, or
//! is a fish older than 3.4 (whose parser refuses the operand) has its task
//! marked `stalled why=shell`, and nothing is typed ([`stall_task`]). A fish that has said it
//! is older than 3.4 is refused before the next worker is created
//! ([`fish_known_old`]).
//!
//! THE TASK NEVER RIDES THE REQUEST LINE. `spawn … len=<n>` is a frame in two
//! steps: the server answers the line `GO <n>`
//! (`aterm_types::control_verbs::task_frame_go`), and only then does the
//! client send the n bytes — so a server that does not know the frame answers
//! the line with an `ERR` and never reads a byte of a task as requests. The
//! serve loop reads the n bytes after the line and carries them, on its own
//! thread, to the one dispatch that serves that line ([`CarriedTask`]); the
//! spawn handler takes them there ([`take_frame`]).

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

/// The longest task a worker is given: 64 KiB, raw, the client's cap too. One
/// argument of that size is far inside every kernel's argument bound (Linux's
/// per-argument `MAX_ARG_STRLEN` is 128 KiB).
pub(crate) const TASK_MAX: usize = aterm_types::control_verbs::MAX_TASK_BYTES;

/// How long a worker's shell has to draw its first prompt before its task is
/// `stalled why=shell`: a login shell's rc files run first, and one that
/// stops on a question of its own (an update prompt) never draws one.
pub(crate) const FIRST_PROMPT_WAIT: Duration = Duration::from_secs(20);

/// The child deadline for `fish --version` ([`fish_says`]); complete-output
/// capture also retains the helper's bounded drain/reap reserves.
const VERSION_WAIT: Duration = Duration::from_secs(5);

/// The reply to a worker's spawn that carried no task frame.
const NO_FRAME: &str = "ERR spawn agent=claude: the task follows the line as a frame \
                        (len=<n>, then n bytes); `aterm ctl spawn agent=claude -` reads it from stdin\n";

thread_local! {
    /// The task frame the serve loop read for the request this thread is
    /// serving ([`CarriedTask`]).
    static FRAME: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// The task a `spawn … len=<n>` frame carried, held for the ONE dispatch that
/// serves its line (`control::serve_request_line`) and cleared when that
/// dispatch ends, so no other request on the thread ever sees it.
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

/// The task frame the request being served carried, taken: the spawn handler
/// reads it once ([`TaskOrder::of_frame`]).
pub(crate) fn take_frame() -> Option<Vec<u8>> {
    FRAME.with(|frame| frame.borrow_mut().take())
}

/// The bytes `len=<v>` announces, or the reply to a length the server will
/// not read — past [`TASK_MAX`], or no length at all — after which the
/// connection closes, since the client already sent what it announced.
pub(crate) fn frame_len(v: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(n) if n <= TASK_MAX => Ok(n),
        _ if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => Err(too_large(v)),
        _ => Err(crate::control::control_media::SPAWN_USAGE.to_string()),
    }
}

/// The refusal of a task over [`TASK_MAX`].
fn too_large(len: &str) -> String {
    format!("ERR spawn agent=claude: the task is {len} bytes (max {TASK_MAX})\n")
}

/// What the control thread hands the main thread for a worker
/// (`Wake::SpawnSession`'s `task`): checked, but not yet written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskOrder {
    /// The task as the frame carried it.
    pub(crate) task: Vec<u8>,
    /// `model=`: the worker's `--model`.
    pub(crate) model: Option<String>,
    /// `from=`, the session that starts the worker (the client's own, which
    /// the spawn's `@<sid>` aim need not be): the task's `from=`, and the
    /// Claude Code whose program, home and login the worker's line carries.
    /// `None`: `from=ctl`.
    pub(crate) caller: Option<SessionId>,
    /// The `cwd=` named, canonical: the one folder the window harness's
    /// folder-trust rule trusts for this worker (`SessionMeta::trusted_cwd`).
    /// `None` when none was named, or its canonical form could not be read
    /// in time ([`crate::spawn_folder::canonical_folder`]): the harness then
    /// asks rather than trusts.
    pub(crate) trusted_cwd: Option<String>,
    /// Whether the task file ends with the [`fact_line`] naming its starter:
    /// a client's spawn's does; the menu bar's worker launch's does not,
    /// since no client starts it or waits on its result, and its brief says
    /// so itself.
    pub(crate) fact_line: bool,
    /// The newborn's own Autopilot word (`spawn … autopilot=<land|cruise>`),
    /// written beside its trusted folder before its agent runs
    /// ([`WORKER_AUTOPILOT`] where the spawn names none): under Land the
    /// worker's one wake at the end is its landing (`done why=landed`), under
    /// Cruise it works on and publishes no `done`.
    pub(crate) autopilot: Autopilot,
}

/// A WORKER'S AUTOPILOT where its spawn names none: Land (FOREVER LAND). A
/// tasked worker's natural end is "land it", which keeps the manager's one
/// wake at the end (`await task`, then `result`): the record reads `done
/// why=landed` once all of its work is on the remote main branch. Cruise is
/// the spawn's `autopilot=cruise`, for open-ended work that no report ends.
pub(crate) const WORKER_AUTOPILOT: Autopilot = Autopilot::Land;

impl TaskOrder {
    /// This order under the spawn's `autopilot=` word (`None`:
    /// [`WORKER_AUTOPILOT`]).
    #[must_use]
    pub(crate) fn under(self, autopilot: Option<Autopilot>) -> Self {
        Self {
            autopilot: autopilot.unwrap_or(WORKER_AUTOPILOT),
            ..self
        }
    }

    /// The order a worker's spawn makes of its `frame` ([`take_frame`]):
    /// `Err` is the reply. An absent or empty task is refused, and so is one
    /// holding a NUL byte, which no argument can carry.
    pub(crate) fn of_frame(
        frame: Option<Vec<u8>>,
        model: Option<String>,
        cwd: Option<&str>,
        caller: Option<SessionId>,
    ) -> Result<Self, String> {
        let Some(task) = frame else {
            return Err(NO_FRAME.to_string());
        };
        if task.is_empty() {
            return Err("ERR spawn agent=claude: the task is empty\n".to_string());
        }
        if task.len() > TASK_MAX {
            return Err(too_large(&task.len().to_string()));
        }
        if task.contains(&0) {
            return Err(
                "ERR spawn agent=claude: the task holds a NUL byte, which no argument can carry\n"
                    .to_string(),
            );
        }
        // Bounded as the spawn's own folder check is: a folder on a hung
        // mount trusts nothing rather than park this control lane.
        let trusted_cwd = cwd.and_then(crate::spawn_folder::canonical_folder);
        Ok(Self {
            task,
            model,
            caller,
            trusted_cwd,
            fact_line: true,
            autopilot: WORKER_AUTOPILOT,
        })
    }
}

/// The ONE LINE OF FACT a worker's task gains (design §2.1; never an order —
/// the primer's rule): who started it, under which Autopilot, what ends it
/// and what becomes of its report, and where a need goes. Under Land the
/// work ends once all of it is on the remote main branch, defended in a few
/// lines starting `Nothing left:`, and that report is the result; under
/// Cruise no report ends it. No retired token (FOREVER CRUISE / FOREVER
/// LAND).
pub(crate) fn fact_line(from: &str, autopilot: Autopilot) -> String {
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
        "(Started by `{from}` with `spawn agent=claude`, {end}. A line starting `NEEDS YOU:` \
         reaches `{from}` and the person while the work goes on.)"
    )
}

/// What a worker's task file holds: the task as given, its last line ended,
/// then — with a starter `from` — a blank line and the [`fact_line`] naming
/// it and its `autopilot`. The shell's `$(…)` trims the trailing newline, so
/// the agent's argv is the task, the blank line and the fact.
pub(crate) fn task_text(task: &[u8], from: Option<&str>, autopilot: Autopilot) -> Vec<u8> {
    let mut text = task.to_vec();
    if !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    if let Some(from) = from {
        text.push(b'\n');
        text.extend_from_slice(fact_line(from, autopilot).as_bytes());
        text.push(b'\n');
    }
    text
}

/// A worker's task file, ready to be written once the newborn's sid is known
/// (`spawn::spawn_session`, on the spawner thread a tab's shell starts on —
/// `tab_spawn`). A clone is the same seed: what one wrote, another discards,
/// so the main thread that booked the worker removes the file the spawner
/// wrote when the worker does not start.
#[derive(Clone)]
pub(crate) struct TaskSeed {
    text: Arc<[u8]>,
    /// The write and explicit discard share one decision across clones.
    written: Arc<Mutex<TaskFileState>>,
}

#[derive(Default)]
struct TaskFileState {
    discarded: bool,
    path: Option<PathBuf>,
}

impl TaskSeed {
    /// The seed of a file holding `text` ([`task_text`]).
    pub(crate) fn new(text: Vec<u8>) -> Self {
        Self {
            text: text.into(),
            written: Arc::default(),
        }
    }

    /// Remove the task file this seed wrote, if it wrote one: its worker did
    /// not start, so nothing will read it. One the close already removed
    /// ([`retire_task_file`]) is no error. Sticky even before a write: a
    /// spawner still running after cancellation must not leave a later file.
    pub(crate) fn discard(&self) {
        let written = {
            let mut state = self.written.lock().unwrap_or_else(|p| p.into_inner());
            state.discarded = true;
            state.path.take()
        };
        if let Some(path) = written
            && let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            aterm_log::warn!("task file {}: not removed: {e}", path.display());
        }
    }

    /// Write the task file of session `sid`, `<state>/tasks/<sid>-1.txt` (a
    /// new record is rev 1): 0600 in a 0700 directory this user owns, never
    /// over a file already there; its path is the shell's `ATERM_TASK_FILE`.
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
    /// A cancellation on either side of the write refuses the spawn; a late
    /// file is removed through the same pinned directory that created it.
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
        let mut state = self.written.lock().unwrap_or_else(|p| p.into_inner());
        if state.discarded {
            drop(state);
            file.remove_exact()?;
            return Err(cancelled());
        }
        state.path = Some(path.clone());
        Ok(path)
    }
}

/// The name of session `sid`'s task file in the tasks directory: a new
/// record is rev 1.
fn task_file_name(sid: &SessionId) -> String {
    format!("{}-1.txt", sid.as_str())
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

/// Remove session `sid`'s task files once the session is gone
/// (`App::retire_session_registration`, the tail of every close; a quit's
/// [`retire_task_files_at_quit`]): its task file, whose task its agent took
/// as argv at its start, and every result its harness snapshotted
/// (`<sid>-<rev>.result.md` and `.result.src`, one pair per done), which
/// `result` reads through the record and the record dies with the session.
/// The worker's report can hold its tools' output, so none outlives it. A
/// seamless update closes nothing, so its successor's worker keeps them
/// (its carried record names its result); ones left by an instance that
/// ended without closing its sessions or quitting (a crash) stay under
/// `<state>/tasks/`.
pub(crate) fn retire_task_file(sid: &SessionId) {
    if let Some(dir) = crate::task_record::tasks_dir() {
        retire_task_file_in(&dir, sid);
    }
}

/// Remove the task files ([`retire_task_file`]) of every session `store`
/// still holds with a task record, at a QUIT (Cmd-Q, `invoke Quit`, `aterm ctl quit`, a headless
/// SIGTERM): the final exit path hangs every shell up and closes no session
/// one by one, so no close removes them ([`retire_task_file`]). A seamless
/// update's Commit exits before that path, so its successor's workers keep
/// theirs; a worker the next launch restores starts a fresh shell and reads
/// none. A session in `parked` is still the parked predecessor's (an update
/// candidate that quits before its Commit: the parent resumes it on the
/// rollback), and so is its file: kept, as its shell is left unhung.
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

/// The dialect a worker's line is written in, for the shell `program` the
/// spawn runs: zsh, bash or fish (fish's version is asked apart,
/// [`fish_known_old`] and [`fish_answer`]). `Err` names the shell, as the
/// refusal and a worker launch's grey row say it
/// (`control_media::WorkerBlock::Shell`).
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
    /// `model=`.
    pub(crate) model: Option<String>,
    /// The starter's session, whose Claude Code the line follows
    /// ([`worker_claude_of`]); `None` for `from=ctl`, or a starter another
    /// instance hosts.
    pub(crate) caller: Option<Arc<SessionCtx>>,
    /// Whether the line carries the starter's Claude home and login, or its
    /// program alone ([`carries_starters_login`]).
    pub(crate) carries_login: bool,
}

/// Start the worker's launcher: on a thread of its own, wait for the shell's
/// first prompt mark and type the one line ([module](self)). A thread that
/// cannot start stalls the task at once.
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
    let (caller, found) = worker_claude_of(work, work.carries_login);
    let line = aterm_agent::harness::relaunch::task_launch_line(
        work.dialect,
        &caller.exe,
        work.model.as_deref(),
        caller.home.as_deref(),
        caller.homedir.as_deref(),
        caller.login.as_deref(),
    );
    match line {
        Ok(line) => {
            let Some(proxy) = work.proxy.as_ref() else {
                stall(work, "its main-thread launch endpoint is gone");
                return;
            };
            // The person can take over while the caller/kernel reads above run.
            // The final gate and press therefore run on the main thread, where
            // Take Over and the person's input cannot interleave with them.
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
                    // A missing reply does not prove no byte landed. Never
                    // retry the initial command or claim this was never typed.
                    note_launch(
                        &work.ctx,
                        format!("outcome not observed ({why}); not retried"),
                    );
                    return;
                }
            };
            let note = format!(
                "typed ({} B, {written}): {} ({}{})",
                line.len() + 1,
                caller.exe.display(),
                found.words(),
                if caller.home.is_some() || caller.homedir.is_some() || caller.login.is_some() {
                    ", with its Claude home and login"
                } else {
                    ""
                },
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
/// — for at most [`FIRST_PROMPT_WAIT`], woken by its output. `Err` says why
/// not. `A` alone (`prompt`) is not enough: zsh's integration marks it from
/// `precmd`, and the rest of `precmd` and the prompt's expansion still run on
/// a cooked tty after it, where a hook that reads the tty (a cursor-position
/// query) would take part of a line typed then.
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

/// Admit and type the worker's initial line ON THE MAIN THREAD. A person
/// Take Over or input handled before this wake is therefore visible here.
/// Resolve the same live launch, refuse a hold or parked update, then use
/// the existing atomic screen/person check-and-press with no delayed retry.
/// No registry lock is held while taking the terminal or fabric leaf locks.
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

/// Whether a worker's line carries its starter's Claude home, `HOME` and login: only
/// when the worker runs under its starter's own identity, `worker` and
/// `starter` the two sessions' identity labels (`None` for both is the
/// human's own agent config). A worker under another one — `identity=<name>`,
/// `identity=-`, or an aimed session's — has that identity's Claude home in
/// its shell's env (`agent_identity::env`, or none), and the starter's, put
/// in front of the line, would override it for the agent alone: the worker
/// would run on the starter's account while `sessions` showed the identity
/// asked for. Then the line carries the starter's program alone
/// ([`line_claude`]), and the identity's own env stands.
pub(crate) fn carries_starters_login(worker: Option<&str>, starter: Option<&str>) -> bool {
    worker == starter
}

/// What a worker's line carries of the Claude Code its starter runs
/// (`found`): its program, and its Claude home, `HOME` and login only when
/// `login` ([`carries_starters_login`]); plain `claude` under the shell's own when
/// the starter runs none.
fn line_claude(
    found: Option<aterm_agent::harness::relaunch::CallerClaude>,
    login: bool,
) -> aterm_agent::harness::relaunch::CallerClaude {
    let mut claude = found.unwrap_or_else(|| aterm_agent::harness::relaunch::CallerClaude {
        exe: PathBuf::from("claude"),
        home: None,
        homedir: None,
        login: None,
    });
    if !login {
        claude.home = None;
        claude.homedir = None;
        claude.login = None;
    }
    claude
}

/// Where a worker's line looked for its starter's Claude Code
/// ([`starter_group`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StarterGroup {
    /// The status sweep named the starter's program Claude Code: its group.
    Named(i32),
    /// The sweep had not named the starter's program yet: the group in front
    /// of the starter's terminal now, followed only where it runs a Claude
    /// Code (`aterm_agent::harness::relaunch::caller_claude` reads its argv).
    Front(i32),
}

/// THE STARTER'S GROUP a worker's line follows: the one the status sweep
/// named, where its reader is Claude Code; none where it named another
/// agent's program (a worker is a Claude Code, and follows a starter's own
/// Claude Code alone); and where it has named NONE yet, the group in front
/// of the starter's terminal now (`front`).
///
/// WHY THE FRONT GROUP (the workers lab, 2026-10-18: 3 of 40 runs, and every
/// run of `lab workers --early` on main's build): a manager whose first turn
/// spawns its workers can ask within half a second of its composer, before
/// the sweep has read its screen. The line then carried plain `claude` and
/// no Claude home, which the worker's login shell did not find (`zsh: command
/// not found: claude`): no Claude Code started, the shell held each tab, and
/// the startup deadline stalled all three workers 120 s on, the log silent.
pub(crate) fn starter_group(
    reader: Option<aterm_phase::Program>,
    named: i32,
    front: impl FnOnce() -> Option<i32>,
) -> Option<StarterGroup> {
    match reader {
        Some(aterm_phase::Program::Claude) => Some(StarterGroup::Named(named)),
        Some(
            aterm_phase::Program::Codex
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic,
        ) => None,
        None => front().filter(|g| *g > 0).map(StarterGroup::Front),
    }
}

/// What a worker's line found of its starter's Claude Code, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    /// The status sweep's group ran it.
    Named,
    /// The starter's terminal's front group ran it; the sweep had not named
    /// it yet.
    Front,
    /// None: plain `claude` under the shell's own.
    Plain,
}

impl Found {
    /// The words the launch line's log names its program with.
    fn words(self) -> &'static str {
        match self {
            Self::Named => "the starter's Claude Code",
            Self::Front => {
                "the starter's Claude Code, read from its terminal: the status sweep had not \
                 named it yet"
            }
            Self::Plain => "no Claude Code of a starter's: plain `claude`",
        }
    }
}

/// The program, Claude home and login a worker's line carries: the starter's
/// own Claude Code's when the starter's session runs one
/// (`aterm_agent::harness::relaunch::caller_claude`, of [`starter_group`]),
/// its home and login only when `login` ([`line_claude`]), else plain
/// `claude` under the shell's own — with where it was found.
fn worker_claude_of(
    work: &Launch,
    login: bool,
) -> (aterm_agent::harness::relaunch::CallerClaude, Found) {
    let found = work.caller.as_deref().and_then(|ctx| {
        let (reader, named) = {
            let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let agent = timeline.agent();
            (agent.reader, agent.program_pgid)
        };
        let group = starter_group(reader, named, || starter_front(&work.store, ctx))?;
        let pgid = match group {
            StarterGroup::Named(g) | StarterGroup::Front(g) => g,
        };
        let pid = u32::try_from(pgid).ok().filter(|p| *p > 0)?;
        let claude =
            aterm_agent::harness::relaunch::caller_claude(pid, &aterm_types::dirs::home_dir()?)?;
        Some((
            claude,
            match group {
                StarterGroup::Named(_) => Found::Named,
                StarterGroup::Front(_) => Found::Front,
            },
        ))
    });
    let how = found.as_ref().map_or(Found::Plain, |(_, how)| *how);
    (line_claude(found.map(|(claude, _)| claude), login), how)
}

/// The group in front of the starter's terminal now (`tcgetpgrp` on its PTY
/// master, read under the registry's read guard so the fd is the session's
/// own); `None` for a starter that is gone, or where none can be read.
#[cfg(unix)]
fn starter_front(store: &Store, ctx: &SessionCtx) -> Option<i32> {
    let g = store.read().unwrap_or_else(|p| p.into_inner());
    let master = g
        .by_sid(&ctx.self_id)
        .filter(|h| !matches!(h.state, SessionState::Exited))?
        .master;
    // SAFETY: `tcgetpgrp` on the starter's own PTY master fd, which the
    // registry holds open for the session's life (its guard is held); it
    // takes no pointers.
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

/// Whether the fish at `shell` is KNOWN to be older than 3.4 — it named a
/// version, and that one cannot take a worker's line ([`fish_says`]): the
/// check before a worker is created (`App::worker_shell`, which the menu bar
/// asks on every refresh, so it never waits on a process). A fish not yet
/// asked is not known: its first look starts the question on a thread of its
/// own and answers `false`, and every look after the answer knows. A fish
/// that named no version is not known to be old either; its launcher asks
/// again ([`fish_answer`]) and stalls the task when it still does not say.
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

/// What the fish at `shell` answered [`fish_known_old`]'s question: `None`
/// while it is being asked (or was never asked), `Some(None)` when it named no
/// version, `Some(Some(takes))` when it named one, so a test tells a real
/// answer from a timeout.
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
/// A fish that names no version is read as an old one.
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
/// The helper retains its bounded drain/reap reserves and may leave an owned
/// stream reader alive; this does not establish descendant termination.
fn fish_says(shell: &Path) -> FishSaid {
    let mut cmd = aterm_uds::spawnfd::command(shell);
    cmd.arg("--version");
    let Ok(output) =
        aterm_uds::deadline::output_until_complete_stdout(&mut cmd, Instant::now() + VERSION_WAIT)
    else {
        return FishSaid::Nothing;
    };
    // A version answer is small. Never classify a retained prefix as complete.
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
