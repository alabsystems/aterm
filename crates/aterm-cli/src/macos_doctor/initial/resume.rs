// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one durable, system-owned trigger that resumes an admitted, unfinished setup run with no
//! window and no person (init-on-by-default design §2.1, §6.5): the login job [`LABEL`], a
//! per-user LaunchAgent that runs aterm's hidden resume entry ([`ENTRY`]) at each login and again
//! after any end but its own exit 0 (a crash, a kill, jetsam, a build an update replaced), with
//! launchd's throttle. An aterm without the entry (a rolled-back predecessor, an older build put
//! back) answers it with that exit 0, so a definition one left behind never respawns.
//!
//! Its definition follows the progress record ([`follow`]): written before any record that owes a
//! pass is published, removed after the first that owes none, so whatever stops aterm (a logout,
//! a restart, a crash, a SIGKILL) and in whatever order the run's stages ended, an unfinished run
//! has it. A window holds [`WINDOW_LOCK`] shared while its setup worker runs
//! ([`hold_initial_setup_window`]): the job runs no pass while one does, and gives way at its
//! next stage boundary once one opens (`SetupHooks::gives_way`). A window whose worker returned
//! (its run completed, or its revision moved) holds nothing, so a run reopened after that is the
//! job's at once, and the open window's too once it hears the records change
//! ([`SetupRecordsWatch`]), person stages included; an explicit pass that saves or completes a run
//! (`aterm init`, `aterm doctor --apply`) keeps the job with it ([`keep_after_explicit`]), so one
//! cut short is resumed with no window, no login and no person.

use super::{
    HeldLock, Path, PathBuf, Progress, ensure_person_scope, finished_at, ledger, progress,
    revision_at, saved_plan, state_root,
};
use aterm_secrets::service::{Domain, Launchctl};
use std::time::{Duration, Instant};

/// The login job's launchd label.
pub const LABEL: &str = aterm_types::macos_setup::SETUP_RESUME_LABEL;

/// The hidden word that names the login job's entry ([`ENTRY`]): off the verb roster, as nobody
/// types it.
pub const VERB: &str = "__setup-resume";

/// The login job's arguments (`aterm --version __setup-resume`): an aterm with the entry reads
/// the pair as it, before anything else in its front door; one without it (a rolled-back
/// predecessor, an older build put back, both of which keep this definition) meets `--version`
/// first, which every aterm answers with exit 0 before it reads another word, so the job ends with
/// the one exit launchd does not start it again after, rather than refusing an unknown word with
/// exit 2 at every throttle for the rest of the login.
pub const ENTRY: [&str; 2] = ["--version", VERB];

/// The lock each window holds shared while its setup worker runs ([`hold_initial_setup_window`]).
pub(super) const WINDOW_LOCK: &str = "setup-window.lock";

/// What launchd writes of the job's stdout and stderr, beside the records it resumes. The job's
/// own lines go to aterm.log.
const LOG: &str = "setup-resume.log";

/// How long one `launchctl` call may take before it is called hung.
const LAUNCHCTL_HANG: Duration = Duration::from_secs(30);

/// How long a booted-out definition may take to leave launchd before the new one loads.
const BOOTOUT_SETTLE: Duration = Duration::from_secs(30);

/// Whether `progress` owes a pass the login job can run: an admitted run that is neither
/// completed nor stopped.
pub(super) fn owes(progress: &Progress) -> bool {
    progress.admitted
        && !progress.completed
        && !progress.stopped
        && !ledger::planted(ledger::Defect::NothingOwed)
}

/// What follows the progress record: the login job's definition ([`Job`]), or a test's.
pub(super) trait Trigger {
    /// Write the definition unless it reads so already; whether it changed.
    fn arm(&self) -> Result<bool, String>;
    /// Remove the definition; whether there was one.
    fn disarm(&self) -> Result<bool, String>;
}

/// Publish `progress` by `save` with the login job beside it: its definition written before a
/// record that owes a pass, so no reader ever finds an owed record without it, and removed after
/// one that owes none. Best effort: a definition that cannot be written leaves the record's write
/// to go on (the run goes on in a window as before), and [`keep_initial_setup_resume`], which a
/// window's every pass runs, writes it again and says why it could not.
pub(super) fn follow(
    root: &Path,
    progress: &Progress,
    save: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    match Job::of(root) {
        Some(job) => follow_with(&job, progress, save),
        None => save(),
    }
}

/// [`follow`] through `trigger`.
pub(super) fn follow_with(
    trigger: &dyn Trigger,
    progress: &Progress,
    save: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let owes = owes(progress);
    if owes {
        let _ = trigger.arm();
    }
    save()?;
    if !owes {
        let _ = trigger.disarm();
    }
    Ok(())
}

/// Where one doctor root's login job lives and what it runs.
struct Job {
    /// The home whose `Library/LaunchAgents` launchd loads at its login.
    home: PathBuf,
    /// Where launchd writes the job's stdout and stderr.
    log: PathBuf,
}

impl Job {
    /// The job of the doctor records at `root`, laid out as an account's own
    /// (`<home>/Library/Application Support/aterm/doctor`); `None` for records anywhere else (a
    /// private state root's, a test's bare scratch folder), which no login resumes.
    fn of(root: &Path) -> Option<Self> {
        let layout = Path::new("Library")
            .join("Application Support")
            .join("aterm")
            .join("doctor");
        if !root.ends_with(&layout) {
            return None;
        }
        let home = root.ancestors().nth(4)?;
        home.is_absolute().then(|| Self {
            home: home.to_path_buf(),
            log: root.join(LOG),
        })
    }

    fn plist(&self) -> PathBuf {
        self.home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    /// The definition as it is written now, running [`program`].
    fn text(&self) -> Result<String, String> {
        let program = program(&self.home)?;
        Ok(aterm_secrets::service::plist_text_until_done(
            LABEL,
            &program,
            &ENTRY.map(str::to_owned),
            &self.log,
            &self.home,
            aterm_secrets::service::bundle_id_of(&program).as_deref(),
        ))
    }
}

impl Trigger for Job {
    fn arm(&self) -> Result<bool, String> {
        let text = self.text()?;
        let plist = self.plist();
        if read_held(&plist, text.len()).is_some_and(|held| held == text.as_bytes()) {
            return Ok(false);
        }
        let dir = plist
            .parent()
            .ok_or_else(|| format!("{} has no folder", plist.display()))?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        aterm_machines::port::write_atomic(&plist, text.as_bytes(), 0o644)
            .map_err(|e| format!("{}: {e}", plist.display()))?;
        super::durable_directory(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(true)
    }

    fn disarm(&self) -> Result<bool, String> {
        let plist = self.plist();
        match std::fs::remove_file(&plist) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(format!("{}: {e}", plist.display())),
        }
        if let Some(dir) = plist.parent() {
            super::durable_directory(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(true)
    }
}

/// The definition at `plist`, read only as a regular file of at most `expected` bytes and one
/// more (anything longer differs anyway).
fn read_held(plist: &Path, expected: usize) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(plist)
        .ok()?;
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut bytes = Vec::with_capacity(expected.saturating_add(1));
    file.take(u64::try_from(expected).ok()?.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// The aterm the job runs: the one this process runs as, where it lasts
/// (`aterm_secrets::service::program_for_service`: never a build in a cargo target directory, a
/// disk image or a translocated copy, which go away), or the one a test's scratch account serves
/// it from ([`serve_from`]).
fn program(home: &Path) -> Result<PathBuf, String> {
    if let Some(program) = served(home) {
        return Ok(program);
    }
    aterm_secrets::service::program_for_service().map_err(|why| why.to_string())
}

#[cfg(test)]
static SERVED: std::sync::Mutex<Vec<(PathBuf, PathBuf)>> = std::sync::Mutex::new(Vec::new());

/// A test's scratch account: its login job runs `program`, as the person's runs the aterm their
/// window runs as (a test binary is a build that goes away, which no job names).
#[cfg(test)]
pub(super) fn serve_from(home: &Path, program: &Path) {
    let mut served = SERVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    served.retain(|(at, _)| at != home);
    served.push((home.to_path_buf(), program.to_path_buf()));
}

#[cfg(test)]
fn served(home: &Path) -> Option<PathBuf> {
    SERVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find_map(|(at, program)| (at == home).then(|| program.clone()))
}

#[cfg(not(test))]
fn served(_: &Path) -> Option<PathBuf> {
    None
}

/// The admitted, unfinished run the login job resumes, by revision, or `None` with none: its
/// records owe a pass ([`owes`]) of the plan in force, which neither a completion nor a Stop
/// finished ([`finished_at`]).
///
/// # Errors
/// This process is not the person's, or the records cannot be read.
pub fn initial_setup_resumable() -> Result<Option<u64>, String> {
    ensure_person_scope()?;
    resumable_at(&state_root()?)
}

pub(super) fn resumable_at(root: &Path) -> Result<Option<u64>, String> {
    let (Some(progress), Some(plan)) = (progress(root)?, saved_plan(root)?) else {
        return Ok(None);
    };
    if !owes(&progress) || !progress.belongs_to(&plan) || finished_at(root)? {
        return Ok(None);
    }
    revision_at(root).map(Some)
}

/// What [`keep_initial_setup_resume`] found and did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupResume {
    /// No run owes a pass: the job's definition is gone. A job launchd still runs exits 0 at its
    /// next look, and none starts at the next login.
    Ended,
    /// launchd runs the job, on this build and its definition.
    Running,
    /// launchd held the job idle (it had exited 0, nothing owed then): it starts again.
    Started,
    /// launchd held no job, or one under an older definition (idle, or between its passes): it
    /// loads this one, which starts it (`RunAtLoad`).
    Loaded,
    /// launchd ran the job on a build an update replaced: it starts again, on the build its
    /// definition names now (`kickstart -k`).
    Renewed,
}

/// One `launchctl` call in this user's domains, run or (in a test) made up.
pub(super) type LaunchctlCall<'a> =
    dyn FnMut(Launchctl<'_>) -> Result<std::process::Output, String> + 'a;

/// Keep the login job with the run: while a run owes a pass, its definition written and launchd
/// running it on this build (loaded now, which starts it, or started again after an exit 0), so a
/// crash, a kill or a logout of this process leaves the run to the job; with none, its definition
/// removed. A window keeps it before each pass of its run, holding the run's automatic claim,
/// which the job holds for each of its own passes: the job runs no stage then (it waits for the
/// window to close, [`wait_for_initial_setup_windows`], or gives way at its next boundary), so
/// one that runs an older definition, or a build an update replaced, is started again on this one
/// with nothing cut off. A run admitted in this login has its job at once rather than from the
/// next login.
///
/// # Errors
/// This process is not the person's (a private state root, sudo, another HOME), the records or
/// the definition cannot be read or written, no lasting aterm can name the job (a build in a
/// target directory, a disk image), or launchctl refused.
pub fn keep_initial_setup_resume() -> Result<SetupResume, String> {
    ensure_person_scope()?;
    keep_at(
        &state_root()?,
        &mut live_launchctl,
        &runs_replaced,
        aterm_types::update_boundary::Boundary::process(),
    )
}

/// Whether the process `pid` runs a build an update replaced: its image is no longer the file
/// `program` names. An image that cannot be read is no replacement.
fn runs_replaced(pid: u32, program: &Path) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(|pid| aterm_uds::process::image_matches_file(pid, program))
        == Some(false)
}

/// [`keep_initial_setup_resume`] at `root`, through `launchctl`, reading a running job's image
/// by `replaced` (`pid`, the program its definition names).
pub(super) fn keep_at(
    root: &Path,
    launchctl: &mut LaunchctlCall<'_>,
    replaced: &dyn Fn(u32, &Path) -> bool,
    boundary: &aterm_types::update_boundary::Boundary,
) -> Result<SetupResume, String> {
    let job = Job::of(root).ok_or_else(|| {
        format!(
            "{}: not an account's own setup records, which no login resumes",
            root.display()
        )
    })?;
    if resumable_at(root)?.is_none() {
        job.disarm()?;
        return Ok(SetupResume::Ended);
    }
    let changed = job.arm()?;
    // From launchd's first read to the load, the step is not repeated safely: it holds the update
    // boundary (init-on-by-default design §6.7), taken before anything is read.
    let _boundary = boundary
        .hold(
            aterm_types::update_boundary::GuardedItem::LaunchdBootstrap,
            || Ok::<(), std::convert::Infallible>(()),
        )
        .unwrap_or_else(|never| match never {});
    let held = launchctl(Launchctl::Print(Domain::Gui, LABEL))?;
    if held.status.success() {
        let pid = aterm_secrets::service::print_pid(&String::from_utf8_lossy(&held.stdout));
        match pid {
            Some(pid) if !changed => {
                if !replaced(pid, &program(&job.home)?) {
                    return Ok(SetupResume::Running);
                }
                succeeded(launchctl, Launchctl::Restart(Domain::Gui, LABEL))?;
                return Ok(SetupResume::Renewed);
            }
            None if !changed => {
                succeeded(launchctl, Launchctl::Kickstart(Domain::Gui, LABEL))?;
                return Ok(SetupResume::Started);
            }
            _ => {}
        }
        // A job under an older definition, idle or between its passes: launchd reads a
        // definition only as it loads it.
        let _ = launchctl(Launchctl::Bootout(Domain::Gui, LABEL));
        let settle = Instant::now() + BOOTOUT_SETTLE;
        while launchctl(Launchctl::Print(Domain::Gui, LABEL))?
            .status
            .success()
            && Instant::now() < settle
        {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    succeeded(launchctl, Launchctl::Bootstrap(Domain::Gui, &job.plist()))?;
    Ok(SetupResume::Loaded)
}

/// `call`, refused unless launchctl exits 0, with what it said.
fn succeeded(launchctl: &mut LaunchctlCall<'_>, call: Launchctl<'_>) -> Result<(), String> {
    let out = launchctl(call)?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "launchctl {} ({}): {}",
        aterm_secrets::service::launchctl_argv(aterm_uds::peer::our_uid(), call).join(" "),
        out.status,
        said.trim()
    ))
}

fn live_launchctl(call: Launchctl<'_>) -> Result<std::process::Output, String> {
    let argv = aterm_secrets::service::launchctl_argv(aterm_uds::peer::our_uid(), call);
    let mut command = aterm_uds::spawnfd::command(aterm_secrets::service::LAUNCHCTL);
    command.args(&argv);
    aterm_uds::deadline::output_within(&mut command, LAUNCHCTL_HANG)
        .map_err(|why| format!("launchctl {}: {why}", argv.join(" ")))
}

/// The login job found nothing to resume ([`initial_setup_resumable`]): remove its definition, so
/// none starts at the next login, and say whether the job may end (its exit 0, which launchd
/// does not start it again after). It may not while the records are being written (their writer
/// keeps the definition with them, [`follow`]), or once a run was admitted since: the job reads
/// the records again.
///
/// # Errors
/// This process is not the person's, or the records or the definition cannot be read or removed.
pub fn end_initial_setup_resume() -> Result<bool, String> {
    ensure_person_scope()?;
    end_at(&state_root()?)
}

pub(super) fn end_at(root: &Path) -> Result<bool, String> {
    let Some(_records) = HeldLock::try_take_named(root, "profile.lock")? else {
        return Ok(false);
    };
    if resumable_at(root)?.is_some() {
        return Ok(false);
    }
    if let Some(job) = Job::of(root) {
        job.disarm()?;
    }
    Ok(true)
}

/// A window's shared hold of [`WINDOW_LOCK`] ([`hold_initial_setup_window`]): the login job
/// runs no pass while it lives, and its drop lets the job go on.
#[must_use = "the login job stands down only while the hold lives"]
pub struct SetupWindowHold {
    _lock: HeldLock,
}

/// Hold [`WINDOW_LOCK`] shared while this window's setup worker runs, so the login job runs no
/// pass meanwhile: the worker keeps the hold until it returns (its run completed, or its
/// revision moved). Each worker takes its own, so two in one process overlap, and a run reopened
/// once every worker returned (a widened `aterm init` cut short, a completion whose reread
/// reopened a stage) is the job's at once, until the open window takes it up at the records'
/// change ([`SetupRecordsWatch`]) and holds this again. A process killed outright lets go with it
/// (the kernel ends a dead process's locks), and the job goes on.
///
/// # Errors
/// This process is not the person's, or the lock cannot be opened.
pub fn hold_initial_setup_window() -> Result<SetupWindowHold, String> {
    ensure_person_scope()?;
    hold_window_at(&state_root()?)
}

pub(super) fn hold_window_at(root: &Path) -> Result<SetupWindowHold, String> {
    Ok(SetupWindowHold {
        _lock: HeldLock::take_shared_named(root, WINDOW_LOCK)?,
    })
}

/// Keep the login job with the run an explicit pass (`aterm init`'s whole profile, `aterm doctor
/// --apply`) just saved or completed at `root` ([`keep_initial_setup_resume`]): a run it widened
/// or reopened owes a pass, and launchd starts the job now rather than at the next login, so one
/// cut short (a Ctrl-C, a closed terminal, a preference that did not take) is resumed with no
/// window and no person; the job's pass waits for the explicit pass's profile.lock. A run it
/// finished ends the job. Only for the person's own records: a private state root, another root
/// or a test binary keeps nothing.
///
/// # Errors
/// This process is not the person's, or keeping the job failed: its definition the save wrote
/// still starts it at the next login, and a window's start keeps it sooner.
pub(super) fn keep_after_explicit(root: &Path) -> Result<SetupResume, String> {
    ensure_person_scope()?;
    if state_root()? != root {
        return Err(format!(
            "{}: not this account's setup records",
            root.display()
        ));
    }
    keep_initial_setup_resume()
}

/// Whether a window that runs setup is open now ([`hold_initial_setup_window`]).
///
/// # Errors
/// This process is not the person's, or the lock cannot be opened.
pub fn initial_setup_window_open() -> Result<bool, String> {
    ensure_person_scope()?;
    window_open_at(&state_root()?)
}

pub(super) fn window_open_at(root: &Path) -> Result<bool, String> {
    Ok(HeldLock::try_take_named(root, WINDOW_LOCK)?.is_none())
}

/// Wait until no window that runs setup is open: each one closing, crashing or killed lets go.
///
/// # Errors
/// This process is not the person's, or the lock cannot be opened.
pub fn wait_for_initial_setup_windows() -> Result<(), String> {
    ensure_person_scope()?;
    drop(HeldLock::take_named(&state_root()?, WINDOW_LOCK)?);
    Ok(())
}

/// What an open window whose setup worker returned waits on to take up a run another process
/// admits or reopens meanwhile (`aterm`'s `setup_take_up`): the setup records' folder, into which
/// every record is renamed, so a run saved there (a widened `aterm init` cut short, a completion
/// whose reread reopened a stage, the login job's own passes) moves it. A watch says only "look
/// again": what is owed is read from the records ([`initial_setup_resumable`]).
pub struct SetupRecordsWatch {
    watcher: atpkg::dirwatch::Watcher,
}

impl SetupRecordsWatch {
    /// Watch this account's setup records.
    ///
    /// # Errors
    /// This process is not the person's, or the folder cannot be watched (none yet, a kernel queue
    /// refused): the window then looks on its timer alone.
    pub fn start() -> Result<Self, String> {
        ensure_person_scope()?;
        Self::at(&state_root()?)
    }

    pub(super) fn at(root: &Path) -> Result<Self, String> {
        let watcher = atpkg::dirwatch::Watcher::new().map_err(|why| why.to_string())?;
        watcher
            .add_dir(root)
            .map_err(|why| format!("{}: {why}", root.display()))?;
        Ok(Self { watcher })
    }

    /// Block until the records may have moved (`true`), or `within` passed (`false`).
    ///
    /// # Errors
    /// The kernel queue failed, or the folder went (removed or replaced): this watch hears no
    /// more, and a new one is to start.
    pub fn wait(&self, within: Duration) -> Result<bool, String> {
        let fired = self
            .watcher
            .wait(Some(within))
            .map_err(|why| why.to_string())?;
        if fired.iter().any(|id| self.watcher.is_gone(*id)) {
            return Err("the setup records' folder went".into());
        }
        Ok(!fired.is_empty())
    }
}
