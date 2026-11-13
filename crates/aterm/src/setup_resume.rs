// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm --version __setup-resume`: the login job that resumes an admitted, unfinished setup
//! run with no window and no person (`aterm_cli::macos_doctor::SETUP_RESUME_LABEL`,
//! init-on-by-default design §2.1, §6.5). launchd starts it at each login and again after any end
//! but its own exit 0. While no window runs setup it runs the run's worker in this process, owned
//! by the job ([`super::SetupOwner::Job`]): the stages that run unattended, on the run's own pace
//! and parks, never a person stage, which waits for a window at this Mac. It waits while a window
//! runs setup and gives way to one that opens at its next stage boundary, ends (other than by
//! exit 0) once an update replaced its build, so launchd starts it again on the new one, notes
//! the stages a logout's SIGTERM cuts off, and exits 0 once no run owes a pass.

use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Duration;

use aterm_cli::macos_doctor::SETUP_RESUME_ENTRY as ENTRY;
use aterm_secrets::program_watch::ProgramWatch;

/// The job's end once an update replaced its build: any exit but 0, which launchd answers by
/// starting the job again, on the build its definition names now.
const REPLACED: u8 = 75;

/// Whether `rest`, the words after the program's, are the login job's entry.
pub(super) fn is_entry(rest: &[OsString]) -> bool {
    rest.len() == ENTRY.len() && rest.iter().zip(ENTRY).all(|(word, entry)| word == entry)
}

/// The build this job runs and the path launchd starts it from, read as it starts.
static PROGRAM: OnceLock<ProgramWatch> = OnceLock::new();

/// Whether an update replaced the build this job runs: the path launchd starts it from names
/// another program now. `false` outside the job.
pub(super) fn replaced() -> bool {
    PROGRAM.get().is_some_and(ProgramWatch::replaced)
}

/// The job's entry, from the front door.
pub(super) fn run() -> ExitCode {
    PROGRAM.get_or_init(ProgramWatch::current);
    take_stop_signals();
    aterm_gui::install_session_log();
    if let Some(why) = aterm_cli::macos_doctor::lasting_scope_refusal() {
        aterm_log::info!("setup resume: not this account's run to resume: {why}");
        return ExitCode::SUCCESS;
    }
    resume_with(&mut Live::default())
}

/// A logout's SIGTERM (launchd ends a login's agents so), a `kickstart -k`, a SIGHUP or a SIGINT
/// ends the job as a quit ends a window: the stages its pass runs now are noted as cut off by no
/// crash ([`stopped_by`]), so the next pass gives their begins back rather than parking one a
/// second logout cut off.
fn take_stop_signals() {
    let taken = aterm_uds::process::take_stop_signals("setup-resume-stop", |signal| {
        std::process::exit(stopped_by(
            signal,
            aterm_cli::macos_doctor::note_setup_interrupted,
        ));
    });
    if !taken {
        aterm_log::warn!(
            "setup resume: no thread takes a logout's signal; a stage it cuts off counts as a stop"
        );
    }
}

/// The job's end on `signal`: the stages this process runs are noted by `note` first, then the
/// exit status, other than 0, so launchd starts the job again where the login goes on.
fn stopped_by(signal: i32, note: impl FnOnce() -> Result<(), String>) -> i32 {
    if let Err(why) = note() {
        aterm_log::warn!("setup resume: could not note the stages signal {signal} cuts off: {why}");
    }
    aterm_log::info!("setup resume: signal {signal} ends the login job");
    128 + signal
}

/// What the job reads and does, which a test fakes.
trait Resume {
    /// Whether an update replaced the build this job runs ([`replaced`]).
    fn replaced(&mut self) -> bool;
    /// The run that owes a pass (`aterm_cli::macos_doctor::initial_setup_resumable`).
    fn resumable(&mut self) -> Result<Option<u64>, String>;
    /// Whether a window runs setup now.
    fn window_open(&mut self) -> Result<bool, String>;
    /// Wait until no window runs setup.
    fn wait_for_windows(&mut self) -> Result<(), String>;
    /// Run `revision`'s worker until the run finishes, a newer one replaces it, a window that
    /// runs setup opens, or an update replaces this build; whether it ran a pass.
    fn work(&mut self, revision: u64) -> bool;
    /// Nothing is owed: remove the job's definition, and say whether the job may end
    /// (`aterm_cli::macos_doctor::end_initial_setup_resume`).
    fn end(&mut self) -> Result<bool, String>;
    /// Wait `pause`, or less on a wake, before reading the records again.
    fn pause(&mut self, pause: Duration);
    /// One aterm.log line, said once until another replaces it.
    fn say(&mut self, line: String);
}

/// The job's loop: work the run that owes a pass while no window runs setup, wait for every
/// window that does, end once an update replaced this build (launchd starts the new one), and
/// exit 0 once nothing is owed. A read that fails, an end the records' writer kept from ending,
/// or a worker that ended with its run still owed and no window open is tried again after the
/// run's own schedule, longer after each such look in a row
/// (`aterm_cli::macos_doctor::PASS_RETRY_SCHEDULE`), and afresh after a window ran setup or a
/// worker ran a pass, never given up: the stages' own pace is the run's durable one, which the
/// worker keeps. The job exits 0 only once nothing is owed, so launchd starts it again after
/// anything else.
fn resume_with(host: &mut impl Resume) -> ExitCode {
    let mut tries = 0u32;
    let mut worked = None;
    loop {
        if host.replaced() {
            host.say(
                "setup resume: an update replaced this build; launchd starts the login job again \
                 on the new one"
                    .into(),
            );
            return ExitCode::from(REPLACED);
        }
        let failed = match host.resumable() {
            Ok(None) => match host.end() {
                Ok(true) => {
                    host.say("setup resume: no run owes a pass; the login job ends".into());
                    return ExitCode::SUCCESS;
                }
                Ok(false) => "the setup records changed as the login job ended".to_owned(),
                Err(why) => why,
            },
            Ok(Some(revision)) => match host.window_open() {
                Ok(true) => {
                    host.say(
                        "setup resume: a window runs setup; the login job waits for it to close"
                            .into(),
                    );
                    worked = None;
                    tries = 0;
                    match host.wait_for_windows() {
                        Ok(()) => continue,
                        Err(why) => why,
                    }
                }
                // A worker that ended with its run still owed and no window open (one that
                // opened and closed at once): the next goes after the job's pause.
                Ok(false) if worked == Some(revision) => {
                    worked = None;
                    format!("the worker of run {revision} ended with the run unfinished")
                }
                Ok(false) => {
                    host.say(format!("setup resume: resumes run {revision}"));
                    if host.work(revision) {
                        tries = 0;
                    }
                    worked = Some(revision);
                    continue;
                }
                Err(why) => why,
            },
            Err(why) => why,
        };
        tries = tries.saturating_add(1);
        let pause = aterm_cli::macos_doctor::PASS_RETRY_SCHEDULE.after(tries);
        host.say(format!(
            "setup resume: {failed}; it reads the records again in {}s",
            pause.as_secs()
        ));
        host.pause(pause);
    }
}

/// The job's live host.
#[derive(Default)]
struct Live {
    said: Option<String>,
}

impl Resume for Live {
    fn replaced(&mut self) -> bool {
        replaced()
    }
    fn resumable(&mut self) -> Result<Option<u64>, String> {
        aterm_cli::macos_doctor::initial_setup_resumable()
    }
    fn window_open(&mut self) -> Result<bool, String> {
        aterm_cli::macos_doctor::initial_setup_window_open()
    }
    fn wait_for_windows(&mut self) -> Result<(), String> {
        aterm_cli::macos_doctor::wait_for_initial_setup_windows()
    }
    fn work(&mut self, revision: u64) -> bool {
        super::run_setup_job(revision)
    }
    fn end(&mut self) -> Result<bool, String> {
        aterm_cli::macos_doctor::end_initial_setup_resume()
    }
    fn pause(&mut self, pause: Duration) {
        super::wait_for_setup_wake(pause);
    }
    fn say(&mut self, line: String) {
        if self.said.as_deref() != Some(line.as_str()) {
            aterm_log::info!("{line}");
            self.said = Some(line);
        }
    }
}

#[cfg(test)]
#[path = "setup_resume_tests.rs"]
mod tests;
