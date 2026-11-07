// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Worker launch (`spawn agent=<claude|codex|agy|muse|grok>`) across harnesses, and a program
//! worker's (`spawn program=<p>`, [`crate::program_worker`]).

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
use crate::task_record::{TaskMark, TaskRecord, TaskState};

#[cfg(unix)]
#[path = "task_launch_codex_source.rs"]
mod codex_source;

/// Actual constructor permission/count fixture for the App drain tests only.
/// Advertised-layout bytes are synthetic and never executed; no source grant.
#[cfg(all(test, unix))]
pub(crate) fn pending_codex_constructor_for_drain() -> (
    aterm_tempfile::TempDir,
    Arc<aterm_agent::harness::codex_source_registry::Registry>,
    aterm_agent::harness::account_work::Lifetime,
) {
    codex_source::pending_for_drain()
}

/// The longest task a worker is given: 64 KiB, raw, the client's cap too.
pub(crate) const TASK_MAX: usize = aterm_types::control_verbs::MAX_TASK_BYTES;

/// How long a launcher waiting for its worker's first prompt rests with no ring
/// ([`await_prompt`]): a person's key rings nothing until it echoes.
const FIRST_PROMPT_LOOK: Duration = Duration::from_secs(1);

/// How long a worker's prompt holds one screen generation before its launcher types its line
/// again after a redraw of that prompt refused it ([`type_at_prompt`]): a prompt that keeps
/// repainting gets one attempt per this at most, and one whose clock ticks each second settles
/// between its ticks.
pub(crate) const PROMPT_SETTLE: Duration = Duration::from_millis(250);

/// How long a fish's `--version` is waited for ([`fish_says`]). A unit test's fish is its own
/// stand-in, which answers, so there it is the hang detector.
#[cfg(not(test))]
const VERSION_WAIT: Duration = Duration::from_secs(5);
#[cfg(test)]
const VERSION_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

fn no_frame(form: &str) -> String {
    format!(
        "ERR spawn {form}: the task follows the line as a frame (len=<n>, then n bytes); \
         `aterm ctl spawn {form} -` reads it from stdin\n"
    )
}

thread_local! {
    static FRAME: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Task payload from a `spawn … len=<n>` frame, held only for its request dispatch.
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

/// The task frame the request being served carried, taken: the spawn handler reads it once.
pub(crate) fn take_frame() -> Option<Vec<u8>> {
    FRAME.with(|frame| frame.borrow_mut().take())
}

/// The bytes `len=<v>` announces, or the reply to a length the server will not read, which names
/// the spawn's `form` ([`spawn_form_word`]).
pub(crate) fn frame_len(v: &str, form: &str) -> Result<usize, String> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(crate::control::control_media::spawn_refusal(Some(
            &format!("len={v}"),
        )));
    }
    match v.parse::<usize>() {
        Ok(n) if n <= TASK_MAX => Ok(n),
        _ => Err(too_large(v, form)),
    }
}

fn too_large(len: &str, form: &str) -> String {
    format!("ERR spawn {form}: the task is {len} bytes (max {TASK_MAX})\n")
}

/// The reply to a spawn line whose `agent=` names a retired agent
/// ([`aterm_phase::RETIRED_AGENTS`], Gemini CLI): refused with the agent that
/// replaces it named, never read as another worker.
pub(crate) fn retired_worker(agent: Option<&str>) -> Option<String> {
    let agent = agent?;
    aterm_phase::RETIRED_AGENTS
        .iter()
        .any(|retired| agent.eq_ignore_ascii_case(retired))
        .then(|| format!("ERR spawn agent={agent}: {}\n", aterm_phase::RETIRED_AGENT))
}

/// The worker word a spawn line's `agent=` names.
pub(crate) fn worker_word(agent: Option<&str>) -> &'static str {
    aterm_agent::harness::worker::WORKERS
        .iter()
        .find(|w| Some(w.word) == agent)
        .map_or("claude", |w| w.word)
}

/// The form a spawn line's refusals name it by: `program=<p>` where it names a program (`program`,
/// as the wire spells it), else `agent=<word>` ([`worker_word`]).
pub(crate) fn spawn_form_word(agent: Option<&str>, program: Option<&str>) -> String {
    match program {
        Some(program) => format!("program={}", aterm_codec::tty::one_line(program, 64)),
        None => format!("agent={}", worker_word(agent)),
    }
}

/// [`spawn_form_word`] of a program worker's decoded `program`.
pub(crate) fn program_form(program: &str) -> String {
    spawn_form_word(None, Some(&aterm_codec::pct::encode(program)))
}

/// What the control thread hands the main thread for a worker (`Wake::SpawnSession`'s `task`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskOrder {
    /// The task as the frame carried it.
    pub(crate) task: Vec<u8>,
    /// `agent=`: the harness the worker runs (`aterm_agent::harness::worker::WORKERS`).
    pub(crate) agent: aterm_phase::Program,
    /// `model=`: the worker's `--model`.
    pub(crate) model: Option<String>,
    /// `from=`, the session that starts the worker.
    pub(crate) caller: Option<SessionId>,
    /// The Mac [`Self::caller`] runs on where the spawn came over a fleet drive: the driver's node
    /// (`crate::fabric::serving_driver`), which the task record keeps as its manager's
    /// ([`crate::task_record::TaskRecord::manager_node`]); `None` for a spawn made on this Mac.
    pub(crate) manager_node: Option<String>,
    /// The `cwd=` named, canonical.
    pub(crate) trusted_cwd: Option<String>,
    /// Whether the task file ends with the [`fact_line`] naming its starter.
    pub(crate) fact_line: bool,
    /// Autopilot word (`spawn … autopilot=<land|cruise>`) written before the worker starts.
    pub(crate) autopilot: Autopilot,
    /// Whether the worker drives the person's own browser (Claude Code's `--chrome`): a credit
    /// claim worker's alone ([`crate::harness_host::credit_claim`]); no `spawn` word sets it.
    pub(crate) chrome: bool,
    /// `program=` and its `arg=` words: a program worker's ([`crate::program_worker`]), whose
    /// [`Self::agent`] is [`aterm_phase::Program::Generic`]; `None` for an agent's.
    pub(crate) program: Option<crate::program_worker::ProgramRun>,
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
        let task = task_of(frame, &format!("agent={}", agent.name()))?;
        let trusted_cwd = cwd.and_then(crate::spawn_folder::canonical_folder);
        Ok(Self {
            task,
            agent,
            model,
            caller,
            manager_node: None,
            trusted_cwd,
            fact_line: true,
            autopilot: WORKER_AUTOPILOT,
            chrome: false,
            program: None,
        })
    }

    /// The order a program worker's spawn (`program=`, [`crate::program_worker`]) makes of its
    /// `frame`: the task its program reads on its standard input, as given (no fact line, which
    /// would be input to it), and no Autopilot, which no harness of its reads. `Err` is the reply.
    pub(crate) fn of_program_frame(
        frame: Option<Vec<u8>>,
        run: crate::program_worker::ProgramRun,
        cwd: Option<&str>,
        caller: Option<SessionId>,
    ) -> Result<Self, String> {
        let task = task_of(frame, &program_form(&run.program))?;
        let trusted_cwd = cwd.and_then(crate::spawn_folder::canonical_folder);
        Ok(Self {
            task,
            agent: aterm_phase::Program::Generic,
            model: None,
            caller,
            manager_node: None,
            trusted_cwd,
            fact_line: false,
            autopilot: WORKER_AUTOPILOT,
            chrome: false,
            program: Some(run),
        })
    }
}

/// The task a spawn's `frame` carries, or the refusal of one that is missing, empty, too large or
/// holds a NUL byte, naming the spawn's `form` ([`spawn_form_word`]).
fn task_of(frame: Option<Vec<u8>>, form: &str) -> Result<Vec<u8>, String> {
    let Some(task) = frame else {
        return Err(no_frame(form));
    };
    if task.is_empty() {
        return Err(format!("ERR spawn {form}: the task is empty\n"));
    }
    if task.len() > TASK_MAX {
        return Err(too_large(&task.len().to_string(), form));
    }
    if task.contains(&0) {
        return Err(format!(
            "ERR spawn {form}: the task holds a NUL byte, which no argument can carry\n"
        ));
    }
    Ok(task)
}

/// The ONE LINE OF FACT a worker's task gains (design §2.1; never an order — the primer's rule).
/// Under Land it names the land check only where aterm types one ([`land_check_answered`]).
pub(crate) fn fact_line(from: &str, autopilot: Autopilot, agent: aterm_phase::Program) -> String {
    let end = match autopilot {
        Autopilot::Cruise => format!(
            "under Autopilot Cruise: no report ends the work, and `{from}` reads its progress \
             from this session"
        ),
        Autopilot::Land | Autopilot::Off => {
            let defends = "a reply defends that in a few lines starting `Nothing left:`";
            let ends = match land_check_answered(agent) {
                Some(answer) => format!("{defends} and the agent {answer}"),
                None => defends.to_owned(),
            };
            // Only aterm's own loop reads this word, in the programs it carries.
            let or_fails = if land_check_answered(agent).is_some() {
                "; if you cannot finish it, a reply whose first line starts `Could not finish: <why>` ends \
                 the work as failed and is returned the same way"
            } else {
                ""
            };
            format!(
                "under Autopilot Land: the work ends once all of it is on the remote main branch, \
                 {ends}, and that defense is returned verbatim to `{from}` as your result{or_fails}"
            )
        }
    };
    format!(
        "(Started by `{from}` with `spawn agent={}`, {end}. A line starting `NEEDS YOU:` \
         reaches `{from}` and the person while the work goes on.)",
        agent.name()
    )
}

/// WHETHER ATERM TYPES `agent` THE LAND CHECK, the answer its Land then ends on
/// ([`aterm_primer::LAND_CHECK_ANSWER`]): only aterm's own loop types it (v6 §14.17.13), into the
/// programs it carries. codex-alab supervises itself, its own Land reading the defense, and the
/// loop types nothing into it; no loop carries the others (the post-landing review, 2026-11-01).
const fn land_check_answered(agent: aterm_phase::Program) -> Option<&'static str> {
    use aterm_phase::Program;
    match agent {
        Program::Claude | Program::Agy | Program::Muse => Some(aterm_primer::LAND_CHECK_ANSWER),
        Program::Codex | Program::Grok | Program::Kimi | Program::Generic => None,
    }
}

/// What a worker's task file holds: the task as given (less its blank lines for a worker whose
/// first prompt may hold none), its last line ended, then — with a starter `from` — the
/// [`fact_line`] naming it and its `autopilot`.
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
        text.extend_from_slice(fact_line(from, autopilot, agent).as_bytes());
        text.push(b'\n');
    }
    text
}

fn without_blank_lines(task: &[u8]) -> Vec<u8> {
    task.split_inclusive(|&b| b == b'\n')
        .filter(|line| !String::from_utf8_lossy(line).chars().all(draws_nothing))
        .flatten()
        .copied()
        .collect()
}

fn draws_nothing(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
        )
}

/// A worker's task file, ready to be written once the newborn's sid is known.
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

    /// Remove the task file this seed wrote: its worker did not start.
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

fn task_file_name(sid: &SessionId) -> String {
    format!("{}-1.txt", sid.as_str())
}

/// The trusted-folders sidecar an older aterm wrote beside a Gemini CLI worker's task file; a
/// session handed across the upgrade can still have one, removed with its task file.
fn trust_file_name(sid: &SessionId) -> String {
    format!("{}-1.trust.json", sid.as_str())
}

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

const TASKS_DIR_SCAN: usize = 65_536;

/// Remove session `sid`'s task files once the session is gone: its task file, and its result
/// files unless `keep_results` (a worker aterm closed keeps them while its retired row stands,
/// [`crate::retired_workers`], then [`retire_result_files`]).
pub(crate) fn retire_task_file(sid: &SessionId, keep_results: bool) {
    if let Some(dir) = crate::task_record::tasks_dir() {
        retire_files_in(
            &dir,
            std::slice::from_ref(sid),
            if keep_results {
                Retire::TaskFile
            } else {
                Retire::All
            },
        );
    }
}

/// Remove session `sid`'s result files, its task file gone with its session already.
pub(crate) fn retire_result_files(sid: &SessionId) {
    if let Some(dir) = crate::task_record::tasks_dir() {
        retire_files_in(&dir, std::slice::from_ref(sid), Retire::Results);
    }
}

/// Which of a session's files [`retire_files_in`] removes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Retire {
    /// Its task file (and an older build's trust sidecar beside it) and its result files.
    All,
    /// Its task file alone.
    TaskFile,
    /// Its result files alone.
    Results,
}

/// Remove the task files ([`retire_task_file`]) of every session `store` still holds with a task
/// record, except those in `parked`, at quit.
pub(crate) fn retire_task_files_at_quit(store: &Store, parked: &[SessionId]) {
    if let Some(dir) = crate::task_record::tasks_dir() {
        retire_task_files_in(&dir, store, parked);
    }
}

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
    retire_files_in(dir, &tasked, Retire::All);
}

fn retire_files_in(dir: &Path, sids: &[SessionId], which: Retire) {
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
    if which != Retire::Results {
        for sid in sids {
            remove(std::ffi::OsStr::new(&task_file_name(sid)));
            remove(std::ffi::OsStr::new(&trust_file_name(sid)));
        }
    }
    if which == Retire::TaskFile {
        return;
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
    /// The starter's session, whose agent of the worker's kind the line follows.
    pub(crate) caller: Option<Arc<SessionCtx>>,
    /// [`TaskOrder::chrome`].
    pub(crate) chrome: bool,
    /// What the supervisor may approve on a permission box (`[harness] approve`) when the worker
    /// was registered: a claim worker's mode follows it ([`worker_fresh`]).
    pub(crate) approve: aterm_agent::supervise::config::Approve,
    /// [`TaskOrder::program`]: the program a program worker's line runs instead of an agent's.
    pub(crate) program: Option<crate::program_worker::ProgramRun>,
    /// Account preparation retires after every other launch resource, including an unstarted box.
    pub(crate) account_scope: Option<aterm_agent::harness::account_work::Scope>,
    /// The actual control server's registry, shared with member validation.
    #[cfg(unix)]
    pub(crate) source_registry: Option<Arc<aterm_agent::harness::codex_source_registry::Registry>>,
}

/// Start the worker's launcher.
pub(crate) fn launch_worker(work: Launch) {
    if account_preparation_cancelled(&work) {
        stall_cancelled(&work);
        return;
    }
    let (ctx, local, agent) = (Arc::clone(&work.ctx), work.local, work.agent);
    let started = std::thread::Builder::new()
        .name(format!("aterm-worker-{local}"))
        .spawn(move || run_worker_launch(&work));
    if started.is_err() {
        stall_refused_task(&ctx, local, agent, "its launcher thread could not start");
    }
}

/// How many more times a pooled launch the pool refused is tried before its worker stalls
/// ([`pool_attempts`]).
const POOL_RETRIES: u32 = 3;

/// The pause before a refused pooled launch is tried again; each later one doubles.
const POOL_RETRY_PAUSE: Duration = Duration::from_secs(1);

/// The longest a held launch waits before it looks at its pool again ([`pool_attempts`]): a row
/// the owner signs in, adds, pins or switches rotation off for is seen within this.
const POOL_HOLD_LOOK: Duration = Duration::from_secs(60);

/// What [`pool_attempts`] waits for before it looks again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PoolWait {
    /// A pause after a refusal that may pass: another process's turn at `accounts.lock`, or a
    /// roster, login or selection that changed while the launch was read or prepared.
    Retry(Duration),
    /// Every row of the pool the launch may take is at its limit, the first of them opening at
    /// `until` (Unix seconds): one look's wait, `look` long at most, uncounted.
    Hold {
        until: i64,
        why: String,
        look: Duration,
    },
}

/// A pooled launch `attempt`, tried again where the pool refused it: held while every row it may
/// take is at its limit (`opening` names when the first opens), else up to [`POOL_RETRIES`] more
/// times after a pause from [`POOL_RETRY_PAUSE`] that doubles. A hold looks at the pool again
/// after each wait of [`POOL_HOLD_LOOK`] at most (and at `until`, by `now`, Unix seconds): the
/// launch is tried again once `opening` no longer names that instant, as where a row serves now,
/// the pool or its mode changed, or another row opens first. `wait` waits, and answers false
/// where the launch is no longer wanted (its session ended, or its task moved on), so no further
/// try claims a row for it.
///
/// # Errors
///
/// The last refusal, where the retries are spent, or `wait` gave up.
pub(crate) fn pool_attempts<T>(
    mut attempt: impl FnMut() -> Result<T, String>,
    mut opening: impl FnMut() -> Option<i64>,
    now: impl Fn() -> Option<i64>,
    mut wait: impl FnMut(&PoolWait) -> bool,
) -> Result<T, String> {
    let mut retries = 0;
    loop {
        let why = match attempt() {
            Ok(done) => return Ok(done),
            Err(why) => why,
        };
        let Some(until) = opening() else {
            if retries == POOL_RETRIES {
                return Err(why);
            }
            let pause = POOL_RETRY_PAUSE.saturating_mul(1 << retries);
            retries += 1;
            if !wait(&PoolWait::Retry(pause)) {
                return Err(why);
            }
            continue;
        };
        loop {
            let left = now().map_or(0, |now| until.saturating_sub(now)).max(0);
            // At least a second, so a clock that reads `until` early never spins the hold.
            let look = Duration::from_secs(left.unsigned_abs().max(1)).min(POOL_HOLD_LOOK);
            let held = PoolWait::Hold {
                until,
                why: why.clone(),
                look,
            };
            if !wait(&held) {
                return Err(why);
            }
            if opening() != Some(until) {
                break;
            }
        }
    }
}

/// When `cli`'s pool at `seat` can next take a launch of the program `starter` runs
/// ([`aterm_agent::harness::accounts_pool::launch_opening`], judged for `starter.exe`, so on a
/// Codex build that keeps no row's login its own only the default row's wall end counts), the
/// instant [`pool_attempts`] holds a refused launch until; `None` for an agent with no pool.
fn pool_opening(
    seat: &aterm_agent::harness::accounts_pool::Seat<'_>,
    starter: &aterm_agent::harness::worker::Starter,
    cli: &aterm_agent::harness::worker::WorkerCli,
) -> Option<i64> {
    use aterm_agent::harness::{accounts, accounts_pool};
    let pool = accounts::Cli::of(cli.agent)?;
    accounts_pool::launch_opening(seat, pool, Some(&starter.exe))
}

pub(crate) fn pool_wait_for<'a>(
    work: &'a Launch,
    held_because: &'static str,
) -> impl FnMut(&PoolWait) -> bool + 'a {
    let alive = move || session_alive(work);
    let mut mark: Option<PoolHoldMark<'a>> = None;
    let mut noted = None;
    let mut sub = None;
    move |next| {
        let wait = match next {
            PoolWait::Retry(pause) => *pause,
            PoolWait::Hold { until, why, look } => {
                if noted != Some(*until) {
                    noted = Some(*until);
                    let until_words =
                        aterm_types::rfc3339::format_rfc3339(u64::try_from(*until).unwrap_or(0));
                    aterm_log::info!(
                        "worker session {}: launch held until {until_words}: {held_because}",
                        work.local
                    );
                    note_launch(
                        &work.ctx,
                        format!("held until {until_words} ({why}): {held_because}"),
                    );
                }
                mark.get_or_insert_with(|| PoolHoldMark::of(&work.ctx))
                    .until(*until, held_because);
                *look
            }
        };
        let sub = sub
            .get_or_insert_with(|| crate::session_watch::Subscription::single(&work.ctx.watchers));
        let now = Instant::now();
        let end = now
            .checked_add(wait)
            .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600));
        loop {
            // A stall the deadline marked ends a wait as it ends the wait for a prompt: another
            // try could claim a row or install a program, and a line typed once the row opens
            // would start a worker nobody is waiting for.
            if !alive() || task_moved_on(&work.ctx).is_some() {
                return false;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            let _ = sub.wait(left);
        }
    }
}

/// [`pooled_starter`] through [`pool_attempts`] for `work`: a hold is noted on its launch record
/// and marks its session ([`PoolHoldMark`]) until the launch starts or stalls, so the startup
/// deadline names the hold; each wait ends early where the session ends or its task moved on.
fn pooled_starter_held(
    work: &Launch,
    starter: aterm_agent::harness::worker::Starter,
    cli: &aterm_agent::harness::worker::WorkerCli,
) -> Result<aterm_agent::harness::worker::Starter, String> {
    pool_launch_attempts(
        |pick| pooled_starter(starter.clone(), cli, work.local, pick),
        || pool_opening(&PoolSeat::here()?.seat(), &starter, cli),
        PoolSeat::unix_now,
        pool_wait_for(work, "no account of its pool can take it now"),
    )
}

/// One worker invocation keeps its read allowance across every retry and hold.
fn pool_launch_attempts<T>(
    mut attempt: impl FnMut(&mut aterm_agent::harness::accounts_pool::LaunchPick) -> Result<T, String>,
    opening: impl FnMut() -> Option<i64>,
    now: impl Fn() -> Option<i64>,
    wait: impl FnMut(&PoolWait) -> bool,
) -> Result<T, String> {
    let mut pick = aterm_agent::harness::accounts_pool::LaunchPick::for_worker_launch();
    pool_attempts(|| attempt(&mut pick), opening, now, wait)
}

/// Where a pooled launch reads its pool ([`aterm_agent::harness::accounts_pool::Seat`]): this
/// user's config directory, harness state, home and the time now.
pub(crate) struct PoolSeat {
    config_dir: PathBuf,
    state: PathBuf,
    home: PathBuf,
    now: i64,
}

impl PoolSeat {
    pub(crate) fn here() -> Option<PoolSeat> {
        Some(PoolSeat {
            config_dir: aterm_types::dirs::aterm_config_path()?
                .parent()?
                .to_path_buf(),
            state: crate::harness_host::harness_state()?,
            home: aterm_types::dirs::home_dir()?,
            now: PoolSeat::unix_now()?,
        })
    }

    fn unix_now() -> Option<i64> {
        aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok())
    }

    pub(crate) fn seat(&self) -> aterm_agent::harness::accounts_pool::Seat<'_> {
        aterm_agent::harness::accounts_pool::Seat {
            config_dir: &self.config_dir,
            state: &self.state,
            home: &self.home,
            now: self.now,
        }
    }
}

pub(crate) fn native_google_agent(agent: aterm_phase::Program) -> bool {
    use aterm_phase::Program;
    match agent {
        Program::Agy => true,
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Muse
        | Program::Generic => false,
    }
}

struct NativeGoogleAdmission {
    #[cfg(unix)]
    worker: Option<aterm_agent::harness::accounts_pool::GooglePoolWorker>,
}

impl NativeGoogleAdmission {
    fn prepare(
        agent: aterm_phase::Program,
        starter: aterm_agent::harness::worker::Starter,
        scope: Option<aterm_agent::harness::account_work::Scope>,
    ) -> Result<(aterm_agent::harness::worker::Starter, Self), aterm_agent::harness::worker::Refused>
    {
        #[cfg(unix)]
        if native_google_agent(agent) {
            let parts = PoolSeat::here().ok_or("Native Google pool location is unavailable")?;
            let program = aterm_agent::harness::login_drive::program_of(
                aterm_agent::harness::accounts::Cli::Agy,
                &parts.home,
            );
            let worker = aterm_agent::harness::accounts_pool::prepare_google_worker_scoped(
                scope.ok_or("Native Google account preparation has no App admission")?,
                &parts.seat(),
                &program,
                starter.clone(),
            )?;
            let starter = match worker.as_ref() {
                Some(worker) => worker.starter()?.clone(),
                None => starter,
            };
            return Ok((starter, Self { worker }));
        }
        let _ = (agent, scope);
        Ok((
            starter,
            Self {
                #[cfg(unix)]
                worker: None,
            },
        ))
    }

    fn check(&self, admitted: &aterm_agent::harness::worker::Admitted) -> Result<(), String> {
        #[cfg(unix)]
        if let Some(worker) = self.worker.as_ref() {
            return worker.check_admitted(admitted);
        }
        let _ = admitted;
        Ok(())
    }
}

fn run_worker_launch(work: &Launch) {
    if account_preparation_cancelled(work) {
        stall_cancelled(work);
        return;
    }
    if let Some(run) = &work.program {
        run_program_launch(work, run);
        return;
    }
    if work.dialect == Dialect::Fish && !fish_answer(&work.shell) {
        stall_refused(
            work,
            "its fish is older than 3.4 (or did not say its version)",
        );
        return;
    }
    let Some(cli) = aterm_agent::harness::worker::worker_cli(work.agent) else {
        stall_refused(work, "its agent has no worker line");
        return;
    };
    #[cfg(unix)]
    let mut codex_source = match codex_source::Preparation::begin(work) {
        Ok(preparation) => preparation,
        Err(why) => {
            stall_refused(work, &why);
            return;
        }
    };
    let (caller, found) = worker_starter_of(work, cli);
    let caller = match pooled_starter_held(work, caller, cli) {
        Ok(caller) => caller,
        Err(why) => {
            stall_gave_up(work, &why);
            return;
        }
    };
    let admission = admit_prepared_worker(
        cli,
        caller,
        aterm_types::dirs::home_dir().as_deref(),
        &|note| note_launch(&work.ctx, note),
        &|| InstallingMark::of(&work.ctx),
        &AtpkgStore(&work.ctx),
        |admitted| {
            #[cfg(unix)]
            let admitted = match codex_source.as_mut() {
                Some(source) => source
                    .arm(admitted, cli)
                    .map_err(aterm_agent::harness::worker::Refused::SignedOut)?,
                None => admitted,
            };
            admitted.prepare(cli, aterm_types::dirs::home_dir().as_deref(), |starter| {
                let _google_mark =
                    native_google_agent(work.agent).then(|| InstallingMark::of(&work.ctx));
                if account_preparation_cancelled(work) {
                    return Err(PREPARATION_CANCELLED.into());
                }
                NativeGoogleAdmission::prepare(work.agent, starter, work.account_scope.clone())
            })
        },
    );
    if admission.is_err() && account_preparation_cancelled(work) {
        stall_cancelled(work);
        return;
    }
    let Some((admitted, google)) = take_admission(
        &work.ctx,
        work.local,
        work.agent,
        admission,
        crate::harness_host::member_label,
    ) else {
        return;
    };
    let admitted = if matches!(found, Found::Parent { lent: false } | Found::Unread) {
        admitted.past_the_twin(cli, work.model.as_deref())
    } else {
        admitted
    };
    if account_preparation_cancelled(work) {
        stall_cancelled(work);
        return;
    }
    let caller = admitted.starter();
    let rotated = match agy_rotation_held(work, caller) {
        Ok(rotated) => {
            if let Some(pick) = &rotated {
                note_launch(&work.ctx, format!("agy quota: {}", pick.describe()));
            }
            rotated
        }
        Err(why) => {
            note_launch(&work.ctx, format!("agy quota: {why}"));
            stall_gave_up(work, &why);
            return;
        }
    };
    let prompt = match await_prompt(work, Duration::ZERO) {
        Ok(prompt) => prompt,
        Err(why) => {
            stall_unprompted(work, &why);
            return;
        }
    };
    let here = trusted_here(
        cli.trust_flag.is_some(),
        work.trusted_cwd.as_deref(),
        || shell_cwd(&work.store, &work.ctx),
    );
    let model = match &rotated {
        Some(aterm_agent::harness::agy_quota::Pick::Rotate { to, .. }) => Some(to.as_str()),
        _ => work.model.as_deref(),
    };
    // A worker runs with its parent's permissions (owner ruling 2026-11-02), held to its CLI's
    // floor (`aterm_agent::harness::worker::WorkerCli::unnamed_posture`; Claude Code's auto, by
    // `aterm_agent::harness::worker::starter`), the disk-free worker included. A credit claim
    // worker (`work.chrome`) names its own mode and tools ([`worker_fresh`]).
    let config = crate::app_config::config_path();
    let mut fresh = worker_fresh(model, here, work.chrome, work.approve);
    fresh.config = config.as_deref();
    let line = aterm_agent::harness::worker::line(cli, work.dialect, &admitted, &fresh);
    match line {
        Ok(line) => {
            let Some(proxy) = work.proxy.as_ref() else {
                stall_refused(work, "its main-thread launch endpoint is gone");
                return;
            };
            let typed = type_at_prompt(work, prompt, |prompt| {
                if account_preparation_cancelled(work) {
                    return Ok(Err(Untyped::Blocked(PREPARATION_CANCELLED.into())));
                }
                #[cfg(unix)]
                if let Some(source) = codex_source.as_ref()
                    && let Err(why) = source.check(&admitted)
                {
                    return Ok(Err(Untyped::Blocked(why)));
                }
                if let Err(why) = google.check(&admitted) {
                    return Ok(Err(Untyped::Blocked(why)));
                }
                crate::control::control_media::call_main(proxy, |reply| crate::Wake::WorkerLaunch {
                    sid: work.ctx.self_id.clone(),
                    nonce: work.ctx.nonce,
                    prompt,
                    line: line.clone(),
                    reply,
                    account_scope: work.account_scope.clone(),
                })
            });
            let Some(written) = typed else {
                return;
            };
            #[cfg(unix)]
            if let Some(source) = codex_source.as_mut()
                && let Err(why) = source.typed()
            {
                // The write was observed. Do not relabel it untyped or retry it.
                note_launch(&work.ctx, format!("typed; {why}"));
            }
            let note = format!(
                "typed ({} B, {written}): {} ({}{}{}){}",
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
                admitted
                    .kept_off()
                    .map(|said| format!("; {said}"))
                    .unwrap_or_default(),
            );
            aterm_log::info!("worker session {}: launch line {note}", work.local);
            note_launch(&work.ctx, note);
        }
        Err(why) => stall_refused(work, &format!("its launch line cannot be written: {why}")),
    }
}

/// A program worker's launch ([`crate::program_worker`]): its program typed at its first prompt
/// with the task file as its standard input ([`aterm_agent::harness::worker::program_line`]), any
/// fish's version alike, then the watch that concludes its task on its program's exit status
/// ([`crate::program_worker::watch`]). No account pool, install or agent of its kind is read:
/// none is its.
fn run_program_launch(work: &Launch, run: &crate::program_worker::ProgramRun) {
    let line = match aterm_agent::harness::worker::program_line(
        work.dialect,
        Path::new(&run.program),
        &run.args,
    ) {
        Ok(line) => line,
        Err(why) => {
            stall_refused(work, &format!("its launch line cannot be written: {why}"));
            return;
        }
    };
    let Some(proxy) = work.proxy.as_ref() else {
        stall_refused(work, "its main-thread launch endpoint is gone");
        return;
    };
    let prompt = match await_prompt(work, Duration::ZERO) {
        Ok(prompt) => prompt,
        Err(why) => {
            stall(work, &why);
            return;
        }
    };
    let typed = type_at_prompt(work, prompt, |prompt| {
        crate::control::control_media::call_main(proxy, |reply| crate::Wake::WorkerLaunch {
            sid: work.ctx.self_id.clone(),
            nonce: work.ctx.nonce,
            prompt,
            line: line.clone(),
            reply,
            account_scope: work.account_scope.clone(),
        })
    });
    let Some(written) = typed else {
        return;
    };
    let note = format!(
        "typed ({} B, {written}): {} with {} argument(s), its task as its input",
        line.len() + 1,
        run.program,
        run.args.len()
    );
    aterm_log::info!("worker session {}: launch line {note}", work.local);
    note_launch(&work.ctx, note);
    crate::program_worker::watch(work);
}

/// What a worker's line names beside its program: `model`, its folder trusted where `here`,
/// and the person's browser where `chrome` ([`TaskOrder::chrome`]). A credit claim worker, the
/// one `chrome` worker ([`crate::harness_host::credit_claim`]), starts in the mode
/// [`claim_worker_mode`] gives `approve`, whatever the person's Claude Code settings say, and
/// with [`credit_offers::CLAIM_TOOLS`](aterm_agent::harness::credit_offers::CLAIM_TOOLS) its only
/// built-in tools, so no mode lets it run a command, write a file, fetch a page or ask; its
/// approval rule (`claim-worker@v1`) judges any box Claude Code still raises there, and in auto
/// mode, which raises none for a Chrome action, the classifier reads the rules of
/// [`credit_offers::claim_settings`](aterm_agent::harness::credit_offers::claim_settings). Any other
/// worker takes its permissions from its [`Starter`](aterm_agent::harness::worker::Starter), and
/// `approve` is not read for it.
pub(crate) fn worker_fresh(
    model: Option<&str>,
    here: bool,
    chrome: bool,
    approve: aterm_agent::supervise::config::Approve,
) -> aterm_agent::harness::worker::Fresh<'_> {
    aterm_agent::harness::worker::Fresh {
        config: None,
        model,
        trusted_here: here,
        permission_mode: chrome.then(|| claim_worker_mode(approve)),
        chrome,
        tools: chrome.then_some(aterm_agent::harness::credit_offers::CLAIM_TOOLS),
        settings: chrome.then(aterm_agent::harness::credit_offers::claim_settings),
    }
}

/// The permission mode a credit claim worker starts in ([`worker_fresh`]), one of
/// [`aterm_agent::harness::worker::CLAUDE_PERMISSION_MODE`]'s modes: Claude Code's auto mode, so
/// nothing it does waits on a person (owner, 2026-11-04: "started in auto / yolo mode"), except
/// where `[harness] approve = "none"` keeps every approval the person's: there every box is
/// raised (`default`) and handed to them, as the supervisor hands every other box.
pub(crate) const fn claim_worker_mode(
    approve: aterm_agent::supervise::config::Approve,
) -> &'static str {
    match approve {
        aterm_agent::supervise::config::Approve::None => "default",
        aterm_agent::supervise::config::Approve::Safe
        | aterm_agent::supervise::config::Approve::All => "auto",
    }
}

/// The store a worker's program is read from and installed into ([`admit_worker`]).
pub(crate) trait ProgramStore {
    /// Whether `program` is here for a worker's line to run ([`runnable_in`]).
    fn present(&self, program: &str) -> bool;
    /// Why atpkg installs no `program` on this machine, where it does not: a person keeps it off
    /// (`[packages] exclude`, a removal, or a no to unattended installs, [`held_in`]), or no
    /// atpkg here can install it.
    fn held(&self, program: &str) -> Option<String>;
    /// Install `program` by atpkg's verified install.
    ///
    /// # Errors
    ///
    /// What the install said, where it did not finish cleanly.
    fn install(&self, program: &str) -> Result<(), String>;
}

/// atpkg's own store, and its verified install run as `aterm pkg install <program>` runs it, for
/// the launcher of the session it holds.
pub(crate) struct AtpkgStore<'a>(&'a SessionCtx);

impl AtpkgStore<'_> {
    /// The atpkg that installs here, or why none can: none beside aterm, an inert package
    /// manager (no package root key pinned in this build), or no store configured.
    fn installer() -> Result<(std::path::PathBuf, atpkg::store::Layout), String> {
        let Some(atpkg) = crate::co_located_atpkg() else {
            return Err("the package tool is not installed beside aterm".to_string());
        };
        if !atpkg::manager_enabled() {
            return Err(
                "the package manager is inert (no package root key is pinned in this build)"
                    .to_string(),
            );
        }
        let Some(layout) = atpkg::store::resolve_configured() else {
            return Err("no package store is configured".to_string());
        };
        Ok((atpkg, layout))
    }
}

impl ProgramStore for AtpkgStore<'_> {
    fn present(&self, program: &str) -> bool {
        atpkg::store::resolve_configured().is_some_and(|layout| runnable_in(&layout, program))
    }

    fn held(&self, program: &str) -> Option<String> {
        let cfg = atpkg::config::load();
        held_in(program, &cfg, atpkg::store::resolve_from(&cfg).as_ref())
            .or_else(|| Self::installer().err())
    }

    fn install(&self, program: &str) -> Result<(), String> {
        let (atpkg, layout) = Self::installer()?;
        install_with(&atpkg, &layout, program, self.0)
    }
}

/// Whether `layout` holds a `program` a worker's line can run: an active build, or a development
/// link whose tools still run ([`atpkg::linkmode::live_linked_checkout`]) and include one of that
/// name, the word the line types. atpkg's install leaves a link as it stands and says so with
/// success, so a launcher that looked only for a build refused a `codex` that runs (m111,
/// 2026-11-06).
pub(crate) fn runnable_in(layout: &atpkg::store::Layout, program: &str) -> bool {
    use atpkg::linkmode::{linked_tool_names, live_linked_checkout};
    atpkg::ops::active_build_of(layout, program).is_some()
        || (live_linked_checkout(layout, program).is_ok_and(|root| root.is_some())
            && linked_tool_names(layout, program)
                .is_some_and(|tools| tools.iter().any(|tool| tool == program)))
}

/// Run `atpkg`'s verified install of `program` on `layout` for the launcher of `ctx`, whose timeline
/// hears the child's pid as soon as it runs: what a store shared with every other pass says of this
/// install is what that process writes (`crate::harness_host::InstallWork`).
fn install_with(
    atpkg: &Path,
    layout: &atpkg::store::Layout,
    program: &str,
    ctx: &SessionCtx,
) -> Result<(), String> {
    let mut cmd = crate::qos::atpkg_command(atpkg);
    cmd.args(crate::pass_lead(
        aterm_types::dirs::runs_under_private_state_root(),
    ));
    let run = crate::run_atpkg_pass(
        install_argv(program, Some(layout)),
        cmd,
        Some(&crate::spawn::atpkg_child_path()),
        Some(layout),
        false,
        &|_: crate::Wake| {},
        &|pid| {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .set_worker_install_child(pid);
        },
    )
    .map_err(|e| format!("it could not run: {e}"))?;
    if run.clean() {
        return Ok(());
    }
    let why = run.why();
    Err(if why.is_empty() {
        format!("it exited {:?} and said nothing", run.code)
    } else {
        why
    })
}

/// A credit claim's Chrome ([`crate::harness_host::credit_claim::ready`]), which aterm puts in
/// place on a Mac alone.
#[cfg(target_os = "macos")]
impl AtpkgStore<'_> {
    /// Why atpkg's brew installs no cask `cask` here unattended, or `None` where it does: a
    /// person keeps it off as they keep any package off ([`kept_off`]), aterm's brew is off
    /// (`[abrew] enabled = false`, or `abrew off`), or no atpkg here can install
    /// ([`Self::installer`]).
    pub(crate) fn cask_held(cask: &str) -> Option<String> {
        let cfg = atpkg::config::load();
        kept_off(cask, &cfg, atpkg::store::resolve_from(&cfg).as_ref())
            .or_else(|| {
                (!atpkg::config::load_abrew().enabled()).then(|| {
                    "aterm's brew is off (`[abrew] enabled = false` in aterm.toml)".to_string()
                })
            })
            .or_else(|| {
                atpkg::abrew::prefix::Prefix::resolve()
                    .is_disabled()
                    .then(|| {
                        "aterm's brew is off (`abrew off`; `abrew on` turns it on)".to_string()
                    })
            })
            .or_else(|| Self::installer().err())
    }

    /// Install cask `cask` by atpkg's brew, as `aterm pkg abrew install --cask <cask>` does,
    /// waiting for it to end: into its prefix's own `Applications` folder
    /// ([`atpkg::abrew::prefix::Prefix::applications`]).
    ///
    /// # Errors
    ///
    /// Why it did not finish cleanly: no atpkg to run, or what the install said as it failed.
    pub(crate) fn install_cask(cask: &str) -> Result<(), String> {
        let (atpkg, _) = Self::installer()?;
        let mut cmd = crate::qos::atpkg_command(&atpkg);
        cmd.args(crate::pass_lead(
            aterm_types::dirs::runs_under_private_state_root(),
        ))
        .args(["abrew", "install", "--cask", cask])
        .env("PATH", crate::spawn::atpkg_child_path())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::piped());
        let out = cmd.output().map_err(|e| format!("it could not run: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let said = String::from_utf8_lossy(&out.stderr);
        let last = said
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(|line| atpkg::progress::sanitize_for_tty(line, 200));
        Err(match last {
            Some(line) => format!("it exited {:?}: {line}", out.status.code()),
            None => format!("it exited {:?} and said nothing", out.status.code()),
        })
    }
}

/// Why atpkg installs no `program` here by what `cfg` (aterm.toml's `[packages]`) and `layout`
/// (atpkg's store, where one is configured) say, or `None`: the pure policy core of
/// [`AtpkgStore::held`]. A person keeps it off ([`kept_off`]), or atpkg serves no build of it
/// for this machine.
pub(crate) fn held_in(
    program: &str,
    cfg: &atpkg::config::PackagesConfig,
    layout: Option<&atpkg::store::Layout>,
) -> Option<String> {
    kept_off(program, cfg, layout).or_else(|| {
        (!atpkg::vendor_direct::served_here(program))
            .then(|| format!("atpkg serves no {program} build for this machine"))
    })
}

/// Why a person keeps `program` off this machine by what `cfg` and `layout` say ([`held_in`]),
/// or `None`: `[packages] exclude` names it, a person removed it, or atpkg may install nothing
/// unattended here (a person's no to installs, or an inert manager, by atpkg's own reading, the
/// one its update pass completes the default set by:
/// [`atpkg::adoption::unattended_install_off`]). A spawn, and a credit claim's Chrome
/// (`AtpkgStore::cask_held`, on a Mac), are aterm's requests, not the person's, so they
/// install nothing a person kept off.
pub(crate) fn kept_off(
    program: &str,
    cfg: &atpkg::config::PackagesConfig,
    layout: Option<&atpkg::store::Layout>,
) -> Option<String> {
    if cfg.exclude().iter().any(|e| e == program) {
        return Some("`[packages] exclude` in aterm.toml names it".to_string());
    }
    if let Some(layout) = layout {
        match layout.removed_programs() {
            Ok(removed) if removed.contains(program) => {
                return Some(format!(
                    "a person removed it (`aterm pkg install {program}` brings it back)"
                ));
            }
            Err(error) => {
                return Some(format!(
                    "removal decisions are unreadable: {error}; installation is held"
                ));
            }
            Ok(_) => {}
        }
    }
    layout
        .and_then(|layout| atpkg::adoption::unattended_install_off(layout, cfg))
        .map(str::to_string)
}

/// The argv of atpkg's verified install of `program` (after any pass lead): the verb
/// `aterm pkg install <program>` runs, waiting its turn behind a running pass and writing its
/// progress where every pass does.
pub(crate) fn install_argv(
    program: &str,
    layout: Option<&atpkg::store::Layout>,
) -> Vec<std::ffi::OsString> {
    let mut argv: Vec<std::ffi::OsString> = vec!["install".into(), program.into()];
    argv.extend(crate::pass_wait_and_progress_args(layout));
    argv
}

/// ADMIT A WORKER BEFORE ITS FIRST PROMPT IS AWAITED: its sign-in first, then its program
/// ([`aterm_agent::harness::worker::admit`]). A signed-out CLI that would open a sign-in page by
/// itself (Grok Build) is refused before `store` is read or anything is installed. A program
/// atpkg ships that `store` holds no build of (a fresh Mac before atpkg's update pass lands it,
/// or one whose install failed) is installed by atpkg's verified install, with each step told to
/// the launch record by `note`; where a person keeps it off ([`held_in`]: an exclude, a removal,
/// or a no to unattended installs), or no atpkg here can install it, nothing is installed and
/// the launch line says so
/// ([`aterm_agent::harness::worker::Admitted::kept_off`]). While the install runs, the session
/// carries the mark `installing` makes ([`InstallingMark`]), so the startup deadline does not
/// count the install as a start that never came until no work has moved in it for
/// [`crate::harness_host::INSTALL_QUIET_LIMIT`]. The launcher sets the same mark around the Google
/// preparation that follows this (`run_worker_launch`).
///
/// # Errors
///
/// Why no worker can start ([`aterm_agent::harness::worker::Refused`]).
pub(crate) fn admit_worker<M>(
    cli: &aterm_agent::harness::worker::WorkerCli,
    starter: aterm_agent::harness::worker::Starter,
    user: Option<&Path>,
    note: &dyn Fn(String),
    installing: &dyn Fn() -> M,
    store: &dyn ProgramStore,
) -> Result<aterm_agent::harness::worker::Admitted, aterm_agent::harness::worker::Refused> {
    use aterm_agent::harness::worker::{Readiness, admit};
    let admitted = admit(
        cli,
        starter,
        user,
        |program| store.present(program),
        |program| store.held(program),
        |program| {
            note(format!(
                "installing {program} by atpkg's verified install: atpkg's store holds no build \
                 of it yet"
            ));
            let _mark = installing();
            store.install(program)
        },
    )?;
    if *admitted.readiness() == Readiness::Installed {
        note(format!(
            "installed {}; atpkg keeps it current from here",
            cli.word
        ));
    }
    Ok(admitted)
}

/// Install/admit the worker before invoking preparation that needs its native program.
/// The returned authentication guard must survive until the final launch check.
fn admit_prepared_worker<M, T>(
    cli: &aterm_agent::harness::worker::WorkerCli,
    starter: aterm_agent::harness::worker::Starter,
    user: Option<&Path>,
    note: &dyn Fn(String),
    installing: &dyn Fn() -> M,
    store: &dyn ProgramStore,
    prepare: impl FnOnce(
        aterm_agent::harness::worker::Admitted,
    ) -> Result<
        (aterm_agent::harness::worker::Admitted, T),
        aterm_agent::harness::worker::Refused,
    >,
) -> Result<(aterm_agent::harness::worker::Admitted, T), aterm_agent::harness::worker::Refused> {
    prepare(admit_worker(cli, starter, user, note, installing, store)?)
}

/// The task record's `why=` for a worker its launcher refused before its line
/// ([`aterm_agent::harness::worker::Refused`]): `signin` for a login only a person can sign back
/// in, `install` for a program whose install failed, and `startup` for a preparation that failed
/// for any other reason, which a later start of its agent heals as any startup stall.
pub(crate) const fn refused_why(refused: &aterm_agent::harness::worker::Refused) -> &'static str {
    match refused {
        aterm_agent::harness::worker::Refused::SignedOut(_) => crate::task_record::WHY_SIGNIN,
        aterm_agent::harness::worker::Refused::Program(_) => crate::task_record::WHY_INSTALL,
        aterm_agent::harness::worker::Refused::Preparation(_) => "startup",
    }
}

/// A session's launcher is installing its worker's program, or preparing a Google worker's sign-in
/// (`NativeGoogleAdmission::prepare`): set on the session's timeline while it lives, and cleared
/// when it drops, on an error or a panic too
/// ([`crate::session_timeline::SessionTimeline::worker_installing`]). The startup deadline does
/// not judge a worker by its 120 s while this stands, since an install ordinarily ends by itself,
/// in a line or a stall. Once no work of its own has moved for
/// [`crate::harness_host::INSTALL_QUIET_LIMIT`], it stalls the worker naming what holds its screen;
/// the launcher then types nothing when the work ends, and nothing ends atpkg's child
/// (ATERM-KI-1105-2). Its own work is what the atpkg child it spawned writes ([`install_with`]), so
/// a sign-in check, which has no such child, has that limit whole, whatever else moves on the
/// store. Each mark has a clock of its own
/// ([`crate::session_timeline::SessionTimeline::worker_install_mark`]), so no install's silence
/// counts against the sign-in check after it. The mark lives only in this process, so a seamless
/// update's successor, which has no launcher, judges the worker as usual.
pub(crate) struct InstallingMark<'a>(&'a SessionCtx);

impl<'a> InstallingMark<'a> {
    /// Set the mark on `ctx`'s timeline.
    pub(crate) fn of(ctx: &'a SessionCtx) -> Self {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_worker_installing(true);
        Self(ctx)
    }
}

impl Drop for InstallingMark<'_> {
    fn drop(&mut self) {
        self.0
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_worker_installing(false);
    }
}

/// A session's launcher is holding its worker's launch until a pool opens
/// ([`pooled_starter_held`], [`agy_rotation_held`]): set on the session's timeline while it lives,
/// and cleared when it drops, on an error or a panic too
/// ([`crate::session_timeline::SessionTimeline::worker_pool_hold`]). Unlike an install, what it
/// waits for may be days off, so the startup deadline does judge it: the worker stalls naming the
/// hold ([`crate::harness_host::StartupHold::LaunchHeld`]), which ends the wait ([`task_moved_on`]).
pub(crate) struct PoolHoldMark<'a>(&'a SessionCtx);

impl<'a> PoolHoldMark<'a> {
    /// A mark on `ctx`'s timeline, which holds nothing until [`Self::until`] says what for.
    pub(crate) const fn of(ctx: &'a SessionCtx) -> Self {
        Self(ctx)
    }

    /// Say that the hold runs until `until` (Unix seconds), `because` nothing can take it now.
    pub(crate) fn until(&self, until: i64, because: &'static str) {
        self.0
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_worker_pool_hold(Some(crate::session_timeline::WorkerPoolHold {
                until,
                because,
            }));
    }
}

impl Drop for PoolHoldMark<'_> {
    fn drop(&mut self) {
        self.0
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_worker_pool_hold(None);
    }
}

fn note_launch(ctx: &SessionCtx, note: String) {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .note_worker_launch(note);
}

/// The first input-ready prompt of `work`'s shell that holds one screen generation for `settle`
/// from the look that first found it ([`Held`]: no time for the launcher's first look,
/// [`PROMPT_SETTLE`] for its look again after a redraw refused its line), waited for as long as
/// its task is `working`. A shell that is slow to it (a loaded machine, a long rc file, a prompt
/// that keeps repainting) has not failed, so this keeps no clock: the startup deadline
/// ([`crate::harness_host::STARTUP_DEADLINE`]) judges every worker that has not started, whatever
/// holds it, and its stall ends this wait as a lead's mark does ([`task_moved_on`]).
pub(crate) fn await_prompt(work: &Launch, settle: Duration) -> Result<PromptFence, String> {
    let sub = crate::session_watch::Subscription::single(&work.ctx.watchers);
    let mut held = Held::default();
    loop {
        if account_preparation_cancelled(work) {
            return Err(PREPARATION_CANCELLED.to_string());
        }
        if !session_alive(work) {
            return Err(SHELL_ENDED.to_string());
        }
        if let Some(why) = task_moved_on(&work.ctx) {
            return Err(why);
        }
        let prompt = {
            let terminal = crate::term_lock(&work.term);
            let human = work.ctx.human_input.seq();
            if human != work.human {
                return Err("a person used its session before the launch".to_string());
            }
            prompt_fence(&terminal, human)
        };
        let rest = match prompt {
            Some(prompt) => {
                let left = held.look(prompt.generation, Instant::now(), settle);
                if left.is_zero() {
                    return Ok(prompt);
                }
                left
            }
            None => {
                held = Held::default();
                FIRST_PROMPT_LOOK
            }
        };
        let _ = sub.wait(rest);
    }
}

/// The screen generation a launcher's last look found at an input-ready prompt, and since when
/// ([`await_prompt`]).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Held(Option<(crate::control::ScreenGen, Instant)>);

impl Held {
    /// What is left to wait, after a look at `now` that found `generation` at an input-ready
    /// prompt, before that prompt has held for `settle`: zero once it has. A generation other
    /// than the last look's counts from `now`.
    pub(crate) fn look(
        &mut self,
        generation: crate::control::ScreenGen,
        now: Instant,
        settle: Duration,
    ) -> Duration {
        let since = match self.0 {
            Some((held, since)) if held == generation => since,
            _ => now,
        };
        self.0 = Some((generation, since));
        settle.saturating_sub(now.saturating_duration_since(since))
    }
}

/// The shell prompt actually observed by the launcher, after its rc files.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromptFence {
    generation: crate::control::ScreenGen,
    human: u64,
    /// The id of the shell block that prompt opened, which a line typed at it runs in.
    block: u64,
}

/// Capture an input-ready prompt under the caller's terminal lock.
pub(crate) fn prompt_fence(terminal: &Terminal, human: u64) -> Option<PromptFence> {
    terminal
        .all_blocks()
        .last()
        .filter(|b| b.state == aterm_core::terminal::BlockState::EnteringCommand)
        .map(|b| PromptFence {
            generation: crate::control::screen_gen(terminal),
            human,
            block: b.id,
        })
}

/// Why [`type_launch_line`] did not type a worker's line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Untyped {
    /// The prompt the launcher saw was redrawn, or is no longer input-ready, or input queued
    /// before the line had not all gone, and nothing was written: its launcher looks again
    /// ([`type_at_prompt`]).
    Unsettled,
    /// The shell or its live launch ended, a person used it, or its task moved on: `stalled
    /// why=shell` where the record still says `working`.
    Refused(String),
    /// Nothing in the shell or from a person stopped it, and the line could not be typed: `stalled
    /// why=startup`, naming why ([`stall_refused`]), or `why=cancelled` where a shutdown or update
    /// stopped its launch ([`stall_cancelled`]).
    Blocked(String),
}

/// Type `line` at `prompt`, the input-ready prompt `work`'s launcher saw, through `admit` (the
/// main thread's [`type_launch_line`]). A refusal that wrote nothing because that prompt was
/// redrawn first, or input queued before it had not all gone ([`Untyped::Unsettled`]: the new
/// tab's grid settling, an async prompt's or a right prompt's repaint, a report still being
/// written) is no stall: the launcher looks again, for a prompt that holds one
/// screen generation for [`PROMPT_SETTLE`] ([`await_prompt`]), and types there, until that look
/// ends as its first does (the shell ending, a person's use, a task that moved on). Any other
/// refusal stalls the task. Only a refusal that wrote nothing is tried again, so the line is
/// typed once at most.
///
/// How the line was written; `None` where nothing was typed (the task is stalled) or the main
/// thread's answer was not observed (noted, not tried again).
pub(crate) fn type_at_prompt<E: std::fmt::Display>(
    work: &Launch,
    mut prompt: PromptFence,
    mut admit: impl FnMut(PromptFence) -> Result<Result<&'static str, Untyped>, E>,
) -> Option<&'static str> {
    loop {
        match admit(prompt) {
            Ok(Ok(written)) => return Some(written),
            Ok(Err(Untyped::Unsettled)) => {
                aterm_log::info!(
                    "worker session {}: its prompt was redrawn, or input before its line was \
                     still queued, before the line was typed; it is typed at the prompt once \
                     that settles",
                    work.local
                );
                note_launch(
                    &work.ctx,
                    "not yet typed (its prompt was redrawn or input before it was queued; looking \
                     again)"
                        .to_string(),
                );
                match await_prompt(work, PROMPT_SETTLE) {
                    Ok(settled) => prompt = settled,
                    Err(why) => {
                        stall_unprompted(work, &why);
                        return None;
                    }
                }
            }
            Ok(Err(Untyped::Refused(why))) => {
                stall(work, &why);
                return None;
            }
            Ok(Err(Untyped::Blocked(why))) => {
                if account_preparation_cancelled(work) {
                    stall_cancelled(work);
                } else {
                    stall_refused(work, &why);
                }
                return None;
            }
            Err(why) => {
                note_launch(
                    &work.ctx,
                    format!("outcome not observed ({why}); not retried"),
                );
                return None;
            }
        }
    }
}

/// Apply the App account cancellation veto before ordinary worker launch admission.
///
/// # Errors
///
/// Returns [`Untyped::Blocked`] for a cancelled account scope, or the unchanged errors from
/// [`type_launch_line`] for its prompt, identity, human-input and task fences.
pub(crate) fn type_scoped_launch_line(
    store: &Store,
    sid: &aterm_session::SessionId,
    nonce: aterm_session::LaunchNonce,
    prompt: PromptFence,
    line: &str,
    scope: Option<&aterm_agent::harness::account_work::Scope>,
) -> Result<&'static str, Untyped> {
    if scope.is_some_and(|scope| scope.cancelled()) {
        return Err(Untyped::Blocked(PREPARATION_CANCELLED.into()));
    }
    type_launch_line(store, sid, nonce, prompt, line)
}

/// Admit and type the worker's initial line ON THE MAIN THREAD. A program worker's record notes the
/// block its line was written into ([`crate::task_record::note_program_block`]) in the same turn
/// of the main thread, before an update's park can come between, so a successor reads its end
/// from that block alone.
///
/// # Errors
///
/// [`Untyped::Unsettled`] where the screen moved off `prompt`, or input queued before the line
/// had not all gone, before anything was written;
/// [`Untyped::Refused`] where the launch is gone, its task moved on ([`task_moved_on`]) or a
/// person used it; [`Untyped::Blocked`] where its session is parked or halted, or the write was not
/// taken.
pub(crate) fn type_launch_line(
    store: &Store,
    sid: &SessionId,
    nonce: aterm_session::LaunchNonce,
    prompt: PromptFence,
    line: &str,
) -> Result<&'static str, Untyped> {
    use crate::control::InputFence;
    let target = {
        let registry = store.read().unwrap_or_else(|p| p.into_inner());
        registry
            .by_sid(sid)
            .filter(|h| h.nonce == nonce && h.state != SessionState::Exited)
            .map(|h| (h.term.clone(), h.ctx.clone()))
    };
    let refused = |why: &str| Err(Untyped::Refused(why.to_string()));
    let Some((term, ctx)) = target else {
        return refused("its live launch is gone");
    };
    if let Some(why) = task_moved_on(&ctx) {
        return Err(Untyped::Refused(why));
    }
    if ctx.update_parked.is_parked() {
        return Err(Untyped::Blocked(
            "its session is parked for an update".to_string(),
        ));
    }
    if let Some(why) = crate::fabric::halt_refusal(ctx.as_ref(), "send") {
        return Err(Untyped::Blocked(why));
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
        crate::ReadOn::EventLoop,
        None,
    );
    let typed = launch_line_typed(result);
    let program = typed.is_ok() && {
        let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.task().is_some_and(TaskRecord::runs_a_program)
    };
    if program && !crate::task_record::note_program_block(&ctx, prompt.block, Duration::ZERO) {
        aterm_log::warn!(
            "worker session {}: the block its program was typed into was not noted (its record \
             moved on, or an update is committing), so its exit status concludes nothing",
            sid.as_str()
        );
    }
    typed
}

/// What the fenced press of a worker's line ([`type_launch_line`]) did, as its launcher reads it.
/// Only a refusal made before a byte was written, the fence's or one for input queued before the
/// line, is [`Untyped::Unsettled`], the one outcome [`type_at_prompt`] tries again, so a line
/// written whole or in part is never typed twice; a person's use and a write the sink did not
/// take are final.
pub(crate) fn launch_line_typed(
    result: crate::control::GuardedInput,
) -> Result<&'static str, Untyped> {
    use crate::control::GuardedInput;
    use crate::input::Delivery;
    let blocked = |why: &str| Err(Untyped::Blocked(why.to_string()));
    match result {
        // The fence decides both before a byte is written, and input queued before the line
        // that had not all gone left no fence judged and nothing written.
        GuardedInput::Changed | GuardedInput::Skipped | GuardedInput::Queued => {
            Err(Untyped::Unsettled)
        }
        GuardedInput::Person => Err(Untyped::Refused(
            "a person used its session before the launch".to_string(),
        )),
        GuardedInput::Pressed(Delivery::Full) => Ok("written whole"),
        GuardedInput::Pressed(Delivery::PartialInDoubt { .. }) => Ok("written in part"),
        GuardedInput::Pressed(Delivery::BusyZero) => blocked("its input sink is busy"),
        GuardedInput::Pressed(Delivery::Failed) => blocked("its input write failed"),
    }
}

/// Why a worker's line must not be typed because its task record is no longer `working`: a
/// deadline stalled it (a pool hold past the startup deadline, or an install that outran it in
/// another process, say), a lead marked it, or it has no record at all. The lead has then been told the task did not
/// start, and a line typed now would start a worker it does not know about, beside any it
/// spawned again.
pub(crate) fn task_moved_on(ctx: &SessionCtx) -> Option<String> {
    let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    match timeline.task() {
        Some(task) if task.state == TaskState::Working => None,
        Some(task) => Some(format!(
            "its task is {}{} now, so its line was not typed",
            task.state.as_str(),
            task.why
                .map(|why| format!(" why={why}"))
                .unwrap_or_default()
        )),
        None => Some("its session has no task record, so its line was not typed".to_string()),
    }
}

/// Choose an agy worker's model through [`pool_attempts`]: a [`aterm_agent::harness::agy_quota::Pick::Wait`]
/// with a known refill (`opening` returns `Some(until)`) holds the launch until that refill (or an
/// earlier pool change), then reads again; a wait with no refill (`opening` is `None`) retries and
/// returns its refusal.
pub(crate) fn agy_pick_attempts(
    mut rotate: impl FnMut(i64) -> Option<aterm_agent::harness::agy_quota::Pick>,
    mut opening: impl FnMut(i64) -> Option<i64>,
    now: impl Fn() -> Option<i64>,
    wait: impl FnMut(&PoolWait) -> bool,
) -> Result<Option<aterm_agent::harness::agy_quota::Pick>, String> {
    pool_attempts(
        || {
            let pick = rotate(now().unwrap_or(0));
            if let Some(aterm_agent::harness::agy_quota::Pick::Wait { .. }) = &pick {
                return Err(pick
                    .as_ref()
                    .map_or_else(String::new, aterm_agent::harness::agy_quota::Pick::describe));
            }
            Ok(pick)
        },
        || opening(now().unwrap_or(0)),
        &now,
        wait,
    )
}

/// An agy worker's model after quota rotation ([`aterm_agent::harness::agy_quota_store::rotate_worker`]),
/// held through [`agy_pick_attempts`] while every agy model pool is spent: `Ok(None)` for another
/// agent, with rotation off, or when agy's quota could not be read.
fn agy_rotation_held(
    work: &Launch,
    caller: &aterm_agent::harness::worker::Starter,
) -> Result<Option<aterm_agent::harness::agy_quota::Pick>, String> {
    use aterm_phase::Program;
    match work.agent {
        Program::Agy => {}
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Muse
        | Program::Generic => {
            return Ok(None);
        }
    }
    let flags = caller.args.clone().unwrap_or_default();
    agy_pick_attempts(
        |now| {
            aterm_agent::harness::agy_quota_store::rotate_worker(
                work.model.as_deref(),
                &caller.exe,
                &caller.env,
                &flags,
                now,
            )
            .map(|l| l.pick)
        },
        |now| {
            aterm_agent::harness::agy_quota_store::worker_opening(
                work.model.as_deref(),
                &caller.env,
                &flags,
                now,
            )
        },
        PoolSeat::unix_now,
        pool_wait_for(work, "every agy model pool is spent"),
    )
}

fn stall(work: &Launch, why: &str) {
    note_launch(&work.ctx, format!("never typed ({why})"));
    stall_task_shell(&work.ctx, work.local, why);
}

/// Stall a launch whose account preparation a shutdown or update cancelled: `why=cancelled`. No
/// shell or person stopped it, so it raises no attention. The word is its own because no agent ran,
/// and only a launch that holds an account scope can get it: the deadlines read `why=held` as a
/// started worker's stall and reopen it when its screen moves
/// ([`crate::harness_host`]'s `startups_of`), which would leave `working` a task nothing will type.
fn stall_cancelled(work: &Launch) {
    note_launch(&work.ctx, format!("never typed ({PREPARATION_CANCELLED})"));
    stall_task_why(
        &work.ctx,
        work.local,
        crate::task_record::WHY_CANCELLED,
        PREPARATION_CANCELLED,
    );
}

/// A launcher's wait for its prompt ([`await_prompt`]) ended with `why`. A shutdown or update that
/// cancelled its account preparation is [`stall_cancelled`] and blames no shell, whatever `why`
/// says, since [`session_alive`] reads that cancellation as a shell's end; anything else ended the
/// wait from the shell, a person or the task's own record ([`stall`]). A pool's wait ends here too
/// where its shell went first ([`stall_gave_up`]).
fn stall_unprompted(work: &Launch, why: &str) {
    if account_preparation_cancelled(work) {
        stall_cancelled(work);
    } else {
        stall(work, why);
    }
}

/// What session `local`'s launcher keeps of its admission. Admitted, the reason atpkg keeps its
/// program off this Mac, if it does, goes on the timeline for a stall to name
/// ([`crate::harness_host::kept_off_held`]). Refused, its task is `stalled` with [`refused_why`]'s
/// word, and the refusal's cause and the member `on` reads go in `attention=` under the mark's
/// lock, so a dispatcher can place the next worker elsewhere. The launch's note and the log keep
/// the whole words.
pub(crate) fn take_admission<T>(
    ctx: &SessionCtx,
    local: u64,
    agent: aterm_phase::Program,
    admission: Result<
        (aterm_agent::harness::worker::Admitted, T),
        aterm_agent::harness::worker::Refused,
    >,
    on: impl FnOnce() -> Option<String>,
) -> Option<(aterm_agent::harness::worker::Admitted, T)> {
    let refused = match admission {
        Ok(admitted) => {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .set_worker_kept_off(admitted.0.kept_off_why().map(str::to_owned));
            return Some(admitted);
        }
        Err(refused) => refused,
    };
    note_launch(ctx, format!("never typed ({})", refused.why()));
    aterm_log::warn!(
        "worker session {local}: {}; nothing was typed",
        refused.why()
    );
    let word = refused_why(&refused);
    let words = crate::harness_host::StartupHold::Refused {
        agent,
        why: refused.short(),
        word,
    }
    .attention_on(on().as_deref());
    mark_refused(ctx, local, word, &words);
    None
}

/// [`stall`] for a launch its launcher gave up on before typing, for a reason no shell or person
/// caused: `stalled why=startup` at once, with the cause and the member in `attention=` under one
/// lock ([`crate::harness_host::StartupHold::Refused`]), so a dispatcher can place the next worker
/// elsewhere. A record that moved on already says what became of the task and keeps its attention.
fn stall_refused(work: &Launch, why: &str) {
    note_launch(&work.ctx, format!("never typed ({why})"));
    stall_refused_task(&work.ctx, work.local, work.agent, why);
}

/// What a launcher says of a launch a shutdown or update stopped ([`stall_cancelled`]).
const PREPARATION_CANCELLED: &str = "account preparation stopped for shutdown or update";

/// Why a worker's launch never typed where its shell went first.
const SHELL_ENDED: &str = "its shell ended before its line was typed";

/// Whether the App closed this preparation's admitted account epoch.
fn account_preparation_cancelled(work: &Launch) -> bool {
    work.account_scope
        .as_ref()
        .is_some_and(|scope| scope.cancelled())
}

/// Whether `work`'s session is still there and its account preparation may continue.
pub(crate) fn session_alive(work: &Launch) -> bool {
    !account_preparation_cancelled(work)
        && work
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(work.local)
            .is_some_and(|h| !matches!(h.state, SessionState::Exited))
}

/// A launcher that gave up waiting on a pool. A live session means the pool's refusal is why
/// ([`stall_refused`]). Otherwise its shell's end or a shutdown ended that wait, and the refusal is
/// not why ([`stall_unprompted`]).
pub(crate) fn stall_gave_up(work: &Launch, why: &str) {
    if session_alive(work) {
        stall_refused(work, why);
    } else {
        stall_unprompted(work, SHELL_ENDED);
    }
}

/// [`stall_refused`] of session `local` by its parts.
pub(crate) fn stall_refused_task(
    ctx: &SessionCtx,
    local: u64,
    agent: aterm_phase::Program,
    why: &str,
) {
    aterm_log::warn!("worker session {local}: {why}; nothing was typed");
    let word = "startup";
    let words = crate::harness_host::StartupHold::Refused {
        agent,
        why: why.to_owned(),
        word,
    }
    .attention_on(crate::harness_host::member_label().as_deref());
    mark_refused(ctx, local, word, &words);
}

/// Mark session `local`'s `working` task stalled with the `why=` word `word` and `words` in
/// `attention=`, both under one lock. A record that moved on already says what became of the task,
/// and keeps its attention.
fn mark_refused(ctx: &SessionCtx, local: u64, word: &'static str, words: &str) {
    let mut mark = TaskMark::new(TaskState::Stalled);
    mark.why = Some(word);
    let marked = crate::task_record::mark_session_task_with(
        ctx,
        local,
        Some(&mark),
        crate::task_record::AttentionEdit::Raise(words),
        ctx.fabric.fence_wait(),
        |task, _| task.is_some_and(|task| task.state == TaskState::Working),
    );
    if marked.is_err() {
        aterm_log::warn!(
            "worker session {local}: the stall was not marked (an update was committing)"
        );
    }
}

/// [`stall`] of session `local` by its parts: `stalled why=shell`.
pub(crate) fn stall_task_shell(ctx: &SessionCtx, local: u64, why: &str) {
    stall_task_why(ctx, local, "shell", why);
}

/// [`stall`] of session `local` by its parts, with the task record's `why=` word `word`. Only a
/// `working` record takes the mark: one that moved on already says what became of the task
/// ([`task_moved_on`]), and a deadline's stall keeps its attention.
fn stall_task_why(ctx: &SessionCtx, local: u64, word: &'static str, why: &str) {
    aterm_log::warn!("worker session {local}: {why}; nothing was typed");
    let mut mark = TaskMark::new(TaskState::Stalled);
    mark.why = Some(word);
    let marked = crate::task_record::mark_session_task_if(
        ctx,
        local,
        &mark,
        ctx.fabric.fence_wait(),
        |task| task.is_some_and(|task| task.state == TaskState::Working),
    );
    if marked.is_err() {
        aterm_log::warn!(
            "worker session {local}: the stall was not marked (an update was committing)"
        );
    }
}

/// What a worker's line carries of the agent of its kind its starter runs (`found`), else what it types with none (`WorkerCli::unstarted`).
pub(crate) fn line_starter(
    cli: &aterm_agent::harness::worker::WorkerCli,
    found: Option<aterm_agent::harness::worker::Starter>,
) -> aterm_agent::harness::worker::Starter {
    found.unwrap_or_else(|| cli.unstarted())
}

/// WHETHER THE WORKER'S LINE TRUSTS ITS FOLDER FOR THIS RUN.
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

/// THE STARTER'S GROUP a worker's line follows: the one the status sweep named, where its reader
/// is the worker's `agent`.
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
    /// Not known: the session it was asked of could not be read, or its status names the agent and
    /// its process reads as none. The line lends nothing and keeps out of the twin's yolo
    /// ([`aterm_agent::harness::worker::Admitted::past_the_twin`]).
    Unread,
    /// None, in a worker no session started: `lent`, whether the session `ls` names as its parent
    /// lent it flags.
    Parent { lent: bool },
}

impl Found {
    fn words(self, agent: aterm_phase::Program) -> String {
        let product = agent.product();
        match self {
            Self::Named => format!("the starter's {product}"),
            Self::Front => format!(
                "the starter's {product}, read from its terminal: the status sweep had not named \
                 it yet"
            ),
            Self::Plain => format!("no {product} of a starter's: plain `{}`", agent.name()),
            Self::Unread => format!("its starter's {product} could not be read: no flags lent"),
            Self::Parent { lent: true } => {
                format!("no {product} of a starter's: its parent's flags")
            }
            Self::Parent { lent: false } => {
                format!("no {product} of a starter's, and its parent lent no flags")
            }
        }
    }
}

fn worker_starter_of(
    work: &Launch,
    cli: &aterm_agent::harness::worker::WorkerCli,
) -> (aterm_agent::harness::worker::Starter, Found) {
    let found = work
        .caller
        .as_deref()
        .map_or(SessionAgent::None, |ctx| agent_of_session(work, ctx, cli));
    let (mut starter, mut how) = match found {
        SessionAgent::Read(starter, how) => (line_starter(cli, Some(starter)), how),
        SessionAgent::None => (line_starter(cli, None), Found::Plain),
        SessionAgent::Unread => (line_starter(cli, None), Found::Unread),
    };
    // A worker no session started (the harness's own, or a spawn from outside a session) has as
    // its parent the session `ls` names: its program and login stay the worker's own, and its
    // permissions are that session's (owner ruling 2026-11-02), held to its CLI's floor as
    // `aterm_agent::harness::worker::starter` read them (`WorkerCli::unnamed_posture`). A parent
    // of another CLI lends nothing, and the worker keeps the posture its unstarted line names
    // (`WorkerCli::unstarted`). A claim worker names its own mode and tools ([`worker_fresh`]).
    if work.caller.is_none()
        && !work.chrome
        && let Some(ctx) = parent_ctx(work)
    {
        match agent_of_session(work, &ctx, cli) {
            SessionAgent::Read(parent, _) => {
                how = Found::Parent {
                    lent: parent.args.as_ref().is_ok_and(|args| !args.is_empty()),
                };
                starter.args = parent.args;
            }
            SessionAgent::None => {}
            SessionAgent::Unread => how = Found::Unread,
        }
    }
    (starter, how)
}

/// The session `ls` names as this worker's parent, which the tab landing recorded.
fn parent_ctx(work: &Launch) -> Option<Arc<SessionCtx>> {
    let store = work.store.read().unwrap_or_else(|p| p.into_inner());
    let parent = store.by_sid(&work.ctx.self_id)?.parent.clone()?;
    Some(store.by_sid(&parent)?.ctx.clone())
}

/// What reading a session for its agent of a worker's kind came to ([`agent_of_session`]).
enum SessionAgent {
    /// It runs one, which lends this, found so.
    Read(aterm_agent::harness::worker::Starter, Found),
    /// It runs none, as far as is read: another agent is named, no group is in front
    /// ([`starter_front`]: a session that is gone, or one none can be read of), or what is in front
    /// has ended, or was read and is no agent of the kind.
    None,
    /// What it runs is not known: a read failed, or its status names the agent and the group that
    /// names reads as none.
    Unread,
}

/// What session `ctx`'s agent of `cli`'s kind lends a worker's line, and how it was found, with
/// the mode its permission pill shows now where it shows one, a Claude Code starter's route read
/// at its folder and at the worker's own (the spawn's `cwd=`, else where the worker's shell
/// stands), whose project settings may name a gateway the starter's lack.
fn agent_of_session(
    work: &Launch,
    ctx: &SessionCtx,
    cli: &aterm_agent::harness::worker::WorkerCli,
) -> SessionAgent {
    use aterm_agent::harness::worker::StarterRead;
    let (reader, named) = {
        let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let agent = timeline.agent();
        (agent.reader, agent.program_pgid)
    };
    let Some(group) = starter_group(reader, named, || starter_front(&work.store, ctx), cli.agent)
    else {
        return SessionAgent::None;
    };
    let (pgid, found) = match group {
        StarterGroup::Named(g) => (g, Found::Named),
        StarterGroup::Front(g) => (g, Found::Front),
    };
    let Some(pid) = u32::try_from(pgid).ok().filter(|p| *p > 0) else {
        return SessionAgent::Unread;
    };
    let Some(home) = aterm_types::dirs::home_dir() else {
        return SessionAgent::Unread;
    };
    let read = aterm_agent::harness::worker::starter(
        cli,
        pid,
        &home,
        || crate::harness_host::live_screen_rows(&work.store, ctx.self_id.as_str()),
        || {
            work.trusted_cwd
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| shell_cwd(&work.store, &work.ctx))
        },
    );
    match read {
        StarterRead::Read(starter) => SessionAgent::Read(starter, found),
        StarterRead::Unread => SessionAgent::Unread,
        StarterRead::Other if found == Found::Named => SessionAgent::Unread,
        StarterRead::Other => SessionAgent::None,
    }
}

/// `starter` under the login its CLI's pool picks for a launch
/// ([`aterm_agent::harness::accounts_pool::launch_env`], reading only the row it starts on, in its
/// own home on this launcher thread), unless the starter runs on a non-default home, a credential
/// override, or sets the pool's seam variable to a login outside the bound roster (one that does
/// runs on a home or credential of its own, which the pool leaves alone; a starter already on a
/// bound pool row still rotates): every pick removes [`pool_onto`]'s variables, replaces any
/// carried seam variable for a row of its own directory, and drops a carried pool seam variable
/// when moving back to the machine's default row. On a Codex program that keeps no row's
/// `CODEX_AUTH_PATH` login its own ([`aterm_agent::harness::codex_alab::auth_path_unread`]),
/// which would run a row of its own on the default home's login, the pool picks only the row on
/// that login ([`aterm_agent::harness::accounts_pool::launch_env_for`]) and says from that row's
/// own condition why it cannot serve where it cannot, and a bound row of its own the starter
/// carried is refused. A Claude Code worker starts on the row of the Claude roster the host would
/// move its starter's tab to ([`claude_starter_in`]).
fn pooled_starter(
    starter: aterm_agent::harness::worker::Starter,
    cli: &aterm_agent::harness::worker::WorkerCli,
    local: u64,
    pick: &mut aterm_agent::harness::accounts_pool::LaunchPick,
) -> Result<aterm_agent::harness::worker::Starter, String> {
    use aterm_agent::harness::{accounts, accounts_pool};
    let Some(pool) = accounts::Cli::of(cli.agent) else {
        return Ok(starter);
    };
    let Some(parts) = PoolSeat::here() else {
        return Ok(starter);
    };
    let seat = parts.seat();
    if pool == accounts::Cli::Claude {
        return Ok(claude_starter_in(starter, &seat, local));
    }
    pooled_starter_reading(starter, pool, &seat, &accounts_pool::LiveUsage, local, pick)
}

/// A Claude Code worker's `starter` onto the row of the Claude roster the host would move the
/// starter's tab to at its next point ([`aterm_agent::harness::accounts::worker_row`], under
/// `[harness] accounts` as Claude Code's pool reads it): `active` where it is another row and
/// fresh, or pinned and still running; else, where the starter's row is walled (or, under
/// `"early"`, low and to be left early), the row a wall would move it to, so a worker spawned
/// from a Limited tab does not start on its wall. The starter is kept as it is where the host
/// would keep the tab (a canary or a spent rotation bound holding it, a row that still runs
/// under `"wall"`), its login is no roster row's (a non-default home, an unbound login), the
/// roster lists one row, rotation is off for Claude Code, or a read fails (logged: a launch
/// never fails on the accounts' records). The host's own flow moves the worker later, as any tab.
pub(crate) fn claude_starter_in(
    starter: aterm_agent::harness::worker::Starter,
    seat: &aterm_agent::harness::accounts_pool::Seat<'_>,
    local: u64,
) -> aterm_agent::harness::worker::Starter {
    let pick = match claude_pick(&starter, seat) {
        Ok(Some(pick)) => pick,
        Ok(None) => return starter,
        Err(why) => {
            aterm_log::info!(
                "worker session {local}: keeps its starter's Claude Code login: the accounts \
                 could not be read: {why}"
            );
            return starter;
        }
    };
    let cli = aterm_agent::harness::accounts::Cli::Claude;
    match pool_onto(starter.clone(), cli, &pick.1, seat.home) {
        Ok(onto) => {
            aterm_log::info!(
                "worker session {local}: starts under Claude Code account {}, where the host \
                 would move its starter's {}",
                pick.1.label,
                pick.0
            );
            onto
        }
        Err(why) => {
            aterm_log::info!(
                "worker session {local}: keeps its starter's Claude Code login: {why}"
            );
            starter
        }
    }
}

/// The starter's row and the login [`claude_starter_in`] moves a worker to, or `None` where it
/// keeps the starter's.
fn claude_pick(
    starter: &aterm_agent::harness::worker::Starter,
    seat: &aterm_agent::harness::accounts_pool::Seat<'_>,
) -> Result<Option<(String, aterm_agent::harness::accounts_pool::LoginEnv)>, String> {
    use aterm_agent::harness::{accounts, accounts_pool};
    let cli = accounts::Cli::Claude;
    let mode = seat.mode_for(cli);
    let who = accounts::ProcessLogin::of_env_for(
        cli,
        starter.env.iter().map(|(n, v)| (n.as_str(), v.as_str())),
    );
    if !accounts::claude_worker_may_move(mode, &who) {
        return Ok(None);
    }
    let roster = seat.bound_roster(cli)?;
    if !roster.several() || accounts::judged_label(&roster, &who).is_none() {
        return Ok(None);
    }
    let (state, _) = seat.store(cli).load_judged(seat.now, &roster)?;
    let enrolled = accounts::enrolled_for(cli, &roster);
    let Some((from, row)) =
        accounts::claude_worker_move(&roster, &state, &enrolled, &who, mode, seat.now)
    else {
        return Ok(None);
    };
    let env = accounts_pool::login_env(cli, row, seat.home).map_err(|why| why.to_string())?;
    Ok(Some((from.to_owned(), env)))
}

/// Until when (unix seconds) the Claude Code login a worker of `starter` would start on
/// ([`claude_starter_in`]) is at a usage limit, or `None` where it can run: where the host would
/// move it to another row ([`claude_pick`]) it runs there, else its starter's own row is walled
/// where the accounts store reads it
/// [`Limited`](aterm_agent::harness::accounts::Condition::Limited). A login that is
/// no roster row's, and accounts that cannot be read, read as no wall: a launch never fails on
/// the accounts' records, and the wall shows itself in the worker's first reply.
pub(crate) fn claude_worker_wall(
    starter: &aterm_agent::harness::worker::Starter,
    seat: &aterm_agent::harness::accounts_pool::Seat<'_>,
) -> Option<i64> {
    use aterm_agent::harness::accounts;
    if matches!(claude_pick(starter, seat), Ok(Some(_))) {
        return None;
    }
    let cli = accounts::Cli::Claude;
    let who = accounts::ProcessLogin::of_env_for(
        cli,
        starter.env.iter().map(|(n, v)| (n.as_str(), v.as_str())),
    );
    let roster = seat.bound_roster(cli).ok()?;
    let row = roster.row(accounts::judged_label(&roster, &who)?)?;
    let (state, _) = seat.store(cli).load(seat.now).ok()?;
    let enrolled = accounts::enrolled_for(cli, &roster);
    match accounts::condition(row, &state, enrolled.contains(&row.label), seat.now) {
        accounts::Condition::Limited { until } => Some(until),
        accounts::Condition::NotEnrolled
        | accounts::Condition::SignedOut
        | accounts::Condition::Low
        | accounts::Condition::Usable
        | accounts::Condition::Unseen => None,
    }
}

#[cfg(test)]
pub(crate) fn pooled_starter_in(
    starter: aterm_agent::harness::worker::Starter,
    pool: aterm_agent::harness::accounts::Cli,
    seat: &aterm_agent::harness::accounts_pool::Seat<'_>,
    read: aterm_agent::harness::accounts_pool::ReadUsage<'_>,
    local: u64,
) -> Result<aterm_agent::harness::worker::Starter, String> {
    pooled_starter_reading(
        starter,
        pool,
        seat,
        read,
        local,
        &mut aterm_agent::harness::accounts_pool::LaunchPick::for_worker_launch(),
    )
}

fn pooled_starter_reading(
    starter: aterm_agent::harness::worker::Starter,
    pool: aterm_agent::harness::accounts::Cli,
    seat: &aterm_agent::harness::accounts_pool::Seat<'_>,
    read: aterm_agent::harness::accounts_pool::ReadUsage<'_>,
    local: u64,
    pick: &mut aterm_agent::harness::accounts_pool::LaunchPick,
) -> Result<aterm_agent::harness::worker::Starter, String> {
    use aterm_agent::harness::accounts;
    let pairs: Vec<(&str, &str)> = starter
        .env
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let who = accounts::ProcessLogin::of_env_for(pool, pairs.iter().copied());
    // Only Claude Code has a token route; another CLI reads one as outside.
    let own = match who.route {
        accounts::AuthRoute::Stored => true,
        accounts::AuthRoute::VaultToken { .. } => pool == accounts::Cli::Claude,
        accounts::AuthRoute::Outside => false,
    };
    if !who.default_home || !own {
        return Ok(starter);
    }
    let var = pool.accounts().seam.var();
    let mut carried_row = None;
    if starter.env.iter().any(|(name, _)| name == var) {
        let roster = seat.bound_roster(pool)?;
        let Some(label) = accounts::judged_label(&roster, &who) else {
            if let Some(login) = who.login.as_deref() {
                let root = seat.store(pool).logins_dir();
                let path = Path::new(login);
                let owned = path.starts_with(&root)
                    || path
                        .canonicalize()
                        .ok()
                        .zip(root.canonicalize().ok())
                        .is_some_and(|(path, root)| path.starts_with(root));
                if owned {
                    return Err(format!(
                        "{} login {} has no current account binding",
                        pool.id(),
                        path.display()
                    ));
                }
            }
            return Ok(starter);
        };
        carried_row = Some(label.to_string());
    }
    if let Some(pick) = pick.pick(seat, pool, read, Some(&starter.exe))? {
        let onto = pool_onto(starter, pool, &pick, seat.home)?;
        aterm_log::info!(
            "worker session {local}: starts under {} account {}",
            pool.id(),
            pick.label
        );
        return Ok(onto);
    }
    if let Some(label) = carried_row {
        codex_row_runs(&starter, pool, seat, &label)?;
    }
    Ok(starter)
}

/// Why a Codex worker `starter` would run on this machine's default login while the pool counts
/// it as `label`'s: it carries a bound row's own `CODEX_AUTH_PATH`, which the pool picked nothing
/// to replace (rotation off with no pin, or Codex's login kept in the OS keyring, for which the
/// pool picks no row), on a program that would not keep that login its own
/// ([`aterm_agent::harness::codex_alab::auth_path_unread`]); the refusal's advice comes from the
/// default row's own condition ([`aterm_agent::harness::accounts_pool::default_row_now`]), told
/// as under a pool that picked no row, rotation off only where that is why: with Codex's login
/// in the keyring the pool reads no pin, so a pin may stand under rotation off too
/// ([`aterm_agent::harness::codex_alab::only_default_carried`]). The default row's starter,
/// which carries no such variable, and every other CLI pass.
fn codex_row_runs(
    starter: &aterm_agent::harness::worker::Starter,
    pool: aterm_agent::harness::accounts::Cli,
    seat: &aterm_agent::harness::accounts_pool::Seat<'_>,
    label: &str,
) -> Result<(), String> {
    use aterm_agent::harness::{accounts, codex_alab};
    if pool != accounts::Cli::Codex
        || !starter
            .env
            .iter()
            .any(|(name, value)| name == codex_alab::AUTH_PATH_ENV && !value.is_empty())
    {
        return Ok(());
    }
    let Some(why) = codex_alab::auth_path_unread(&starter.exe) else {
        return Ok(());
    };
    let default = aterm_agent::harness::accounts_pool::default_row_now(seat, pool)?;
    Err(format!(
        "{}; no worker was started",
        codex_alab::only_default_carried(
            &why,
            label,
            &default,
            seat.mode_for(pool) == accounts::Mode::Off && !seat.keyring_held(pool)
        )
    ))
}

/// `starter` onto `pool`'s login `pick` ([`pooled_starter`]): the variables
/// [`aterm_agent::harness::relaunch::pool_args`] removes and its flags (Grok Build's own leader
/// socket for a row of its own, in the home the starter carries, else `~/.grok` under the user's
/// `home`), any removed variables cleared from the carried environment, and the selected
/// login seam exported or explicitly unset for the default row. Reapplication replaces the
/// previous pool's removal list so a default row's unset cannot mask a later selected login.
///
/// # Errors
///
/// [`aterm_agent::harness::relaunch::pool_args`]' refusal.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkerEnvironment",
        action = "Provider",
        reason = "Select one of the four native provider fixtures."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkerEnvironment",
        action = "Own",
        reason = "Select the default login or a row with its own login path."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkerEnvironment",
        action = "Carried",
        reason = "Select clean, conflicting or previously pooled starter environments."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountWorkerEnvironment",
        action = "Apply",
        project = "aterm_gui::task_launch::tests::account_worker_env::model::environment_transition"
    )
)]
pub(crate) fn pool_onto(
    mut starter: aterm_agent::harness::worker::Starter,
    pool: aterm_agent::harness::accounts::Cli,
    pick: &aterm_agent::harness::accounts_pool::LoginEnv,
    home: &Path,
) -> Result<aterm_agent::harness::worker::Starter, String> {
    let carried = |name: &str| {
        starter
            .env
            .iter()
            .find(|(n, _)| n == name && n != pick.var)
            .map(|(_, value)| value.clone())
    };
    let cli_home = aterm_agent::ai::facts::home_of(pool.connector(), &carried, Some(home)).ok();
    let flags = starter.args.as_deref().unwrap_or_default();
    let args = aterm_agent::harness::relaunch::pool_args(pool, pick, cli_home.as_deref(), flags)?;
    if let Ok(flags) = starter.args.as_mut() {
        *flags = args.flags;
    }
    starter
        .env
        .retain(|(name, _)| name != pick.var && !args.unset.contains(&name.as_str()));
    starter.unset = args.unset;
    if let Some(value) = pick.value.clone() {
        starter.env.push((pick.var.to_string(), value));
    }
    Ok(starter)
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
    // SAFETY: `tcgetpgrp` on the starter's own PTY master fd, which the registry holds open for the session's life (its guard is held); it takes no pointers.
    let pgrp = unsafe { libc::tcgetpgrp(master) };
    (pgrp > 0).then_some(pgrp)
}

/// No foreground group to read off unix: the sweep's naming alone.
#[cfg(not(unix))]
fn starter_front(_store: &Store, _ctx: &SessionCtx) -> Option<i32> {
    None
}

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

fn fish_answers() -> &'static Mutex<std::collections::HashMap<FishKey, FishSaid>> {
    static ANSWERS: std::sync::OnceLock<Mutex<std::collections::HashMap<FishKey, FishSaid>>> =
        std::sync::OnceLock::new();
    ANSWERS.get_or_init(Mutex::default)
}

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
