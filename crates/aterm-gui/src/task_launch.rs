// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Worker launch (`spawn agent=<claude|codex|agy|muse|grok>`) across harnesses.

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

const VERSION_WAIT: Duration = Duration::from_secs(5);

fn no_frame(agent: &str) -> String {
    format!(
        "ERR spawn agent={agent}: the task follows the line as a frame (len=<n>, then n bytes); \
         `aterm ctl spawn agent={agent} -` reads it from stdin\n"
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

/// The bytes `len=<v>` announces, or the reply to a length the server will not read.
pub(crate) fn frame_len(v: &str, agent: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(n) if n <= TASK_MAX => Ok(n),
        _ if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => Err(too_large(v, agent)),
        _ => Err(crate::control::control_media::spawn_usage()),
    }
}

fn too_large(len: &str, agent: &str) -> String {
    format!("ERR spawn agent={agent}: the task is {len} bytes (max {TASK_MAX})\n")
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
    /// The `cwd=` named, canonical.
    pub(crate) trusted_cwd: Option<String>,
    /// Whether the task file ends with the [`fact_line`] naming its starter.
    pub(crate) fact_line: bool,
    /// Autopilot word (`spawn … autopilot=<land|cruise>`) written before the worker starts.
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
        text.extend_from_slice(fact_line(from, autopilot, agent.name()).as_bytes());
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

/// Remove session `sid`'s task files once the session is gone.
pub(crate) fn retire_task_file(sid: &SessionId) {
    if let Some(dir) = crate::task_record::tasks_dir() {
        retire_task_file_in(&dir, sid);
    }
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
    retire_files_in(dir, &tasked);
}

fn retire_task_file_in(dir: &Path, sid: &SessionId) {
    retire_files_in(dir, std::slice::from_ref(sid));
}

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
    /// The starter's session, whose agent of the worker's kind the line follows.
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

fn run_worker_launch(work: &Launch) {
    if work.dialect == Dialect::Fish && !fish_answer(&work.shell) {
        stall(
            work,
            "its fish is older than 3.4 (or did not say its version)",
        );
        return;
    }
    let Some(cli) = aterm_agent::harness::worker::worker_cli(work.agent) else {
        stall(work, "its agent has no worker line");
        return;
    };
    let (caller, found) = worker_starter_of(work, cli);
    let caller = match pooled_starter(caller, cli, work.local) {
        Ok(caller) => caller,
        Err(why) => {
            stall(work, &why);
            return;
        }
    };
    let admitted = match admit_worker(
        cli,
        caller,
        aterm_types::dirs::home_dir().as_deref(),
        &|note| note_launch(&work.ctx, note),
        &|| InstallingMark::of(&work.ctx),
        &AtpkgStore,
    ) {
        Ok(admitted) => admitted,
        Err(refused) => {
            stall_why(work, refused_why(&refused), refused.why());
            return;
        }
    };
    let caller = admitted.starter();
    let prompt = match await_first_prompt(work) {
        Ok(prompt) => prompt,
        Err(why) => {
            stall(work, why);
            return;
        }
    };
    let here = trusted_here(
        cli.trust_flag.is_some(),
        work.trusted_cwd.as_deref(),
        || shell_cwd(&work.store, &work.ctx),
    );
    let rotated = agy_rotation(work, caller);
    if let Some(pick) = &rotated {
        note_launch(&work.ctx, format!("agy quota: {}", pick.describe()));
        if pick.model().is_none() {
            stall(work, &pick.describe());
            return;
        }
    }
    let model = match &rotated {
        Some(aterm_agent::harness::agy_quota::Pick::Rotate { to, .. }) => Some(to.as_str()),
        _ => work.model.as_deref(),
    };
    let fresh = aterm_agent::harness::worker::Fresh {
        model,
        trusted_here: here,
    };
    let line = aterm_agent::harness::worker::line(cli, work.dialect, &admitted, &fresh);
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
                "typed ({} B, {written}): {} ({}{}{})",
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
            );
            aterm_log::info!("worker session {}: launch line {note}", work.local);
            note_launch(&work.ctx, note);
        }
        Err(why) => stall(work, &format!("its launch line cannot be written: {why}")),
    }
}

/// The store a worker's on-demand program is read from and installed into ([`admit_worker`]).
pub(crate) trait ProgramStore {
    /// Whether `program` has an installed build.
    fn present(&self, program: &str) -> bool;
    /// Why a person keeps `program` off this machine, where they do.
    fn held(&self, program: &str) -> Option<String>;
    /// Install `program` by atpkg's verified install.
    ///
    /// # Errors
    ///
    /// What the install said, where it did not finish cleanly.
    fn install(&self, program: &str) -> Result<(), String>;
}

/// atpkg's own store, and its verified install run as `aterm pkg install <program>` runs it.
pub(crate) struct AtpkgStore;

impl ProgramStore for AtpkgStore {
    fn present(&self, program: &str) -> bool {
        atpkg::store::resolve_configured()
            .is_some_and(|layout| atpkg::ops::active_builds(&layout).contains_key(program))
    }

    fn held(&self, program: &str) -> Option<String> {
        if atpkg::config::load().exclude().iter().any(|e| e == program) {
            return Some("`[packages] exclude` in aterm.toml names it".to_string());
        }
        atpkg::store::resolve_configured()
            .is_some_and(|layout| layout.removed_programs().contains(program))
            .then(|| format!("a person removed it (`aterm pkg install {program}` brings it back)"))
    }

    fn install(&self, program: &str) -> Result<(), String> {
        let Some(atpkg) = crate::co_located_atpkg() else {
            return Err("the package tool is not installed beside aterm".to_string());
        };
        if !atpkg::manager_enabled() {
            return Err(
                "the package manager is inert (no package root key is pinned in this build)"
                    .to_string(),
            );
        }
        let layout = atpkg::store::resolve_configured();
        let mut cmd = crate::qos::atpkg_command(&atpkg);
        cmd.args(crate::pass_lead(
            aterm_types::dirs::runs_under_private_state_root(),
        ));
        let run = crate::run_atpkg_pass(
            install_argv(program, layout.as_ref()),
            cmd,
            Some(&crate::spawn::atpkg_child_path()),
            layout.as_ref(),
            false,
            &|_: crate::Wake| {},
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
/// itself (Grok Build) is refused before anything is installed. A CLI the default set does not
/// keep is installed by atpkg's verified install where it is absent, with each step told to the
/// launch record by `note`. While the install runs, the session carries the mark `installing`
/// makes ([`InstallingMark`]), so the startup deadline does not count the install as a start
/// that never came.
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
                "installing {program} by atpkg's verified install: `spawn agent={}` named it, \
                 which opts this machine in",
                cli.word
            ));
            let _mark = installing();
            store.install(program)
        },
    )?;
    if admitted.readiness() == Readiness::Installed {
        note(format!(
            "installed {}; atpkg keeps it current from here",
            cli.word
        ));
    }
    Ok(admitted)
}

/// The task record's `why=` for a worker its launcher refused before its line
/// ([`aterm_agent::harness::worker::Refused`]): `signin` for a signed-out CLI, which only a
/// person can fix, and `install` for a program that is held back or failed to install.
pub(crate) const fn refused_why(refused: &aterm_agent::harness::worker::Refused) -> &'static str {
    match refused {
        aterm_agent::harness::worker::Refused::SignedOut(_) => crate::task_record::WHY_SIGNIN,
        aterm_agent::harness::worker::Refused::Program(_) => crate::task_record::WHY_INSTALL,
    }
}

/// A session's launcher is installing its worker's program: set on the session's timeline while
/// it lives, and cleared when it drops, on an error or a panic too
/// ([`crate::session_timeline::SessionTimeline::worker_installing`]). The startup deadline does
/// not judge a worker while this stands. It lives only in this process, so a seamless update's
/// successor, which has no launcher, judges the worker as usual.
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

fn note_launch(ctx: &SessionCtx, note: String) {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .note_worker_launch(note);
}

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
    if let Some(why) = task_moved_on(&ctx) {
        return Err(why);
    }
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

/// Why a worker's line must not be typed because its task record is no longer `working`: a
/// deadline stalled it (an install that outran the startup deadline in another process, say), a
/// lead marked it, or it has no record at all. The lead has then been told the task did not
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

/// An agy worker's model after quota rotation ([`aterm_agent::harness::agy_quota_store::rotate_worker`]):
/// `None` for another agent, with rotation off, or when agy's quota could not be read.
fn agy_rotation(
    work: &Launch,
    caller: &aterm_agent::harness::worker::Starter,
) -> Option<aterm_agent::harness::agy_quota::Pick> {
    use aterm_phase::Program;
    match work.agent {
        Program::Agy => {}
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Muse
        | Program::Generic => {
            return None;
        }
    }
    let flags = caller.args.clone().unwrap_or_default();
    let now = aterm_agent::supervise::limit::unix_now();
    aterm_agent::harness::agy_quota_store::rotate_worker(
        work.model.as_deref(),
        &caller.exe,
        &caller.env,
        &flags,
        now,
    )
    .map(|l| l.pick)
}

fn stall(work: &Launch, why: &str) {
    stall_why(work, "shell", why);
}

/// [`stall`] with the task record's `why=` word `word`.
fn stall_why(work: &Launch, word: &'static str, why: &str) {
    note_launch(&work.ctx, format!("never typed ({why})"));
    stall_task_why(&work.ctx, work.local, word, why);
}

/// [`stall`] of session `local` by its parts.
pub(crate) fn stall_task(ctx: &SessionCtx, local: u64, why: &str) {
    stall_task_why(ctx, local, "shell", why);
}

/// [`stall_task`] with the task record's `why=` word `word`.
pub(crate) fn stall_task_why(ctx: &SessionCtx, local: u64, word: &'static str, why: &str) {
    aterm_log::warn!("worker session {local}: {why}; its task is stalled and nothing was typed");
    let mut mark = TaskMark::new(TaskState::Stalled);
    mark.why = Some(word);
    if crate::task_record::mark_session_task(ctx, local, &mark).is_err() {
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
        }
    }
}

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

/// `starter` under the login its CLI's pool picks for a launch
/// ([`aterm_agent::harness::accounts_pool::launch_env`], once each row's usage was read in its
/// own home on this launcher thread), unless the starter runs on a non-default home, a credential
/// override, or sets the pool's seam variable to a login outside the bound roster (one that does
/// runs on a home or credential of its own, which the pool leaves alone; a starter already on a
/// bound pool row still rotates): every pick removes [`pool_onto`]'s variables, replaces any
/// carried seam variable for a row of its own directory, and drops a carried pool seam variable
/// when moving back to the machine's default row. Claude Code's workers keep the starter's login,
/// which the host's own flow moves.
fn pooled_starter(
    starter: aterm_agent::harness::worker::Starter,
    cli: &aterm_agent::harness::worker::WorkerCli,
    local: u64,
) -> Result<aterm_agent::harness::worker::Starter, String> {
    use aterm_agent::harness::{accounts, accounts_pool};
    let Some(pool) = accounts::Cli::of(cli.agent).filter(|c| *c != accounts::Cli::Claude) else {
        return Ok(starter);
    };
    let (Some(config), Some(state), Some(home), Some(now)) = (
        aterm_types::dirs::aterm_config_path(),
        crate::harness_host::harness_state(),
        aterm_types::dirs::home_dir(),
        aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()),
    ) else {
        return Ok(starter);
    };
    let Some(config_dir) = config.parent() else {
        return Ok(starter);
    };
    let seat = accounts_pool::Seat {
        config_dir,
        state: &state,
        home: &home,
        now,
    };
    pooled_starter_in(starter, pool, &seat, &accounts_pool::live_usage, local)
}

pub(crate) fn pooled_starter_in(
    starter: aterm_agent::harness::worker::Starter,
    pool: aterm_agent::harness::accounts::Cli,
    seat: &aterm_agent::harness::accounts_pool::Seat<'_>,
    read: aterm_agent::harness::accounts_pool::ReadUsage<'_>,
    local: u64,
) -> Result<aterm_agent::harness::worker::Starter, String> {
    use aterm_agent::harness::{accounts, accounts_pool};
    let pairs: Vec<(&str, &str)> = starter
        .env
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let who = accounts::ProcessLogin::of_env_for(pool, pairs.iter().copied());
    if !who.default_home || !who.stored_login {
        return Ok(starter);
    }
    let var = pool.accounts().seam.var();
    if starter.env.iter().any(|(name, _)| name == var) {
        let roster = seat.bound_roster(pool)?;
        if accounts::judged_label(&roster, &who).is_none() {
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
        }
    }
    if let Some(pick) = accounts_pool::launch_env(seat, pool, read)? {
        let onto = pool_onto(starter, pool, &pick, seat.home)?;
        aterm_log::info!(
            "worker session {local}: starts under {} account {}",
            pool.id(),
            pick.label
        );
        return Ok(onto);
    }
    Ok(starter)
}

/// `starter` onto `pool`'s login `pick` ([`pooled_starter`]): the variables
/// [`aterm_agent::harness::relaunch::pool_args`] removes and its flags (Grok Build's own leader
/// socket for a row of its own, in the home the starter carries, else `~/.grok` under the user's
/// `home`), any prior seam variable replaced, and a row of its own's seam variable exported.
///
/// # Errors
///
/// [`aterm_agent::harness::relaunch::pool_args`]' refusal.
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
    starter.unset = args.unset;
    starter.env.retain(|(n, _)| n != pick.var);
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
