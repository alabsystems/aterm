// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System Settings' Quit & Reopen. Once a person turns an access on or off for aterm (Full Disk
//! Access, say), System Settings asks aterm to quit, with a logout's quit reason (`rlgo`), waits
//! about three seconds, and reopens only an app that quit by then. aterm answers AppKit's ask to
//! terminate with `NSTerminateLater` (`menu::defer_quit_for_terminate`), asks the person nothing
//! for a quit System Settings asked for (`system_quit::explicit`), and lets AppKit end the process
//! only once its own shutdown is done, which need not fit in that wait (whether it does is not
//! measured); one System Settings asks for while aterm's own alert or panel is open, where no reply
//! could come, AppKit hears cancelled, and aterm quits once the alert or panel is answered, well
//! past that wait. A quit System Settings asked for therefore leaves a helper behind that opens
//! aterm again once this process is gone, and tries `open` again on `OPEN_RETRY`'s schedule while
//! it fails, `OPEN_TRIES` times in all. What the helper did it leaves in `<state>`
//! (`OUTCOME_RECORD`), and the next aterm writes that in its journal (`journal_last_reopen`): the
//! helper has no journal of its own.

#[cfg(any(target_os = "macos", test))]
use aterm_types::retry_schedule::Backoff;
#[cfg(any(target_os = "macos", test))]
use aterm_uds::process::{Birth, Identity, ProcessPresence};
use std::ffi::OsString;
#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", test))]
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

/// `keySenderPIDAttr`, `'spid'`: the pid of the program that sent an Apple event.
#[cfg(any(target_os = "macos", test))]
pub(crate) const KEY_SENDER_PID: u32 = u32::from_be_bytes(*b"spid");

/// The programs whose quit is a Quit & Reopen: the extension System Settings' Privacy & Security
/// pane runs in, which sends it, and System Settings itself.
#[cfg(any(target_os = "macos", test))]
const REOPEN_SENDERS: [&str; 2] = [
    "/System/Library/ExtensionKit/Extensions/SecurityPrivacyExtension.appex/Contents/MacOS/\
     SecurityPrivacyExtension",
    "/System/Applications/System Settings.app/Contents/MacOS/System Settings",
];

/// The hidden verb `aterm __reopen-after <process-token> <bundle>` ([`run_reopen_after`]).
pub const REOPEN_VERB: &str = "__reopen-after";

/// How long the helper waits for the quitting aterm to be gone before it gives up.
#[cfg(target_os = "macos")]
const GONE_CEILING: Duration = Duration::from_secs(120);

/// How long the helper waits for LaunchServices to drop the gone aterm before it opens anyway:
/// LaunchServices answers an `open` for an app it still lists by bringing that app forward.
#[cfg(target_os = "macos")]
const FORGOTTEN_CEILING: Duration = Duration::from_secs(5);

#[cfg(target_os = "macos")]
const FORGOTTEN_TICK: Duration = Duration::from_millis(20);

/// How long one `open` may take.
#[cfg(target_os = "macos")]
const OPEN_CEILING: Duration = Duration::from_secs(60);

/// The pause before each `open` after one that failed (LaunchServices still taking the gone
/// aterm down, say): a second after the first failure, doubling to half a minute.
#[cfg(any(target_os = "macos", test))]
const OPEN_RETRY: Backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));

/// How many times the helper tries `open` in all. With [`OPEN_RETRY`]'s pauses (91 s in all) an
/// `open` LaunchServices refuses at once gives up a minute and a half after the quitting aterm
/// is gone; at the very most, every wait run to its ceiling ([`GONE_CEILING`],
/// `FORGOTTEN_CEILING`, each try's `OPEN_CEILING`), the helper outlives the quit by
/// 120 + 5 + 8 × 60 + 91 s, under twelve minutes.
#[cfg(any(target_os = "macos", test))]
const OPEN_TRIES: u32 = 8;

/// What the last Quit & Reopen helper did, in `<state>`: its unix seconds, its outcome's word
/// ([`Reopened::word`]) and the `open`s it tried, one line, for the next aterm's journal
/// ([`journal_last_reopen`]).
#[cfg(any(target_os = "macos", test))]
const OUTCOME_RECORD: &str = "quit-reopen-outcome";

/// The most [`OUTCOME_RECORD`] holds.
#[cfg(any(target_os = "macos", test))]
const MAX_OUTCOME_BYTES: usize = 256;

/// Whether a quit sent by the program at `path` is System Settings' Quit & Reopen.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn sender_reopens(path: &Path) -> bool {
    REOPEN_SENDERS
        .iter()
        .any(|sender| path == Path::new(sender))
}

/// The arguments of the helper that opens `bundle` again once `target` is gone.
#[cfg(any(target_os = "macos", test))]
fn reopen_args(target: Identity, bundle: &Path) -> [OsString; 3] {
    [
        REOPEN_VERB.into(),
        target.token().into(),
        bundle.as_os_str().to_owned(),
    ]
}

/// The helper's command: the bundle's front door, `Contents/MacOS/aterm`, with [`reopen_args`].
/// Not this process's own path: an aterm started under a window alias (`Contents/MacOS/aterm-gui`)
/// would start the helper under that name too, and the router hands a window alias's arguments
/// to a window, so the verb would never be read.
#[cfg(any(target_os = "macos", test))]
fn reopen_command(target: Identity, bundle: &Path) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(bundle.join("Contents/MacOS/aterm"));
    command.args(reopen_args(target, bundle));
    command
}

/// [`reopen_args`] read back past the verb: a process token and an absolute path to a `.app`.
#[cfg(any(target_os = "macos", test))]
fn parse_reopen_args(args: &[OsString]) -> Option<(Identity, PathBuf)> {
    let [target, bundle] = args else {
        return None;
    };
    let target = Identity::parse(target.to_str()?)?;
    let bundle = PathBuf::from(bundle);
    let app = bundle
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"));
    (bundle.is_absolute() && app).then_some((target, bundle))
}

/// What the helper did.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reopened {
    /// The quitting aterm still ran at the ceiling: nothing was opened.
    StillRunning,
    /// `open` opened the app at its `tries`-th try.
    Opened { tries: u32 },
    /// `open` failed each of its [`OPEN_TRIES`] tries.
    OpenFailed,
}

#[cfg(any(target_os = "macos", test))]
impl Reopened {
    /// The outcome's word in [`OUTCOME_RECORD`].
    const fn word(self) -> &'static str {
        match self {
            Self::StillRunning => "still-running",
            Self::Opened { .. } => "opened",
            Self::OpenFailed => "open-failed",
        }
    }

    /// The `open`s tried.
    const fn tries(self) -> u32 {
        match self {
            Self::StillRunning => 0,
            Self::Opened { tries } => tries,
            Self::OpenFailed => OPEN_TRIES,
        }
    }
}

/// [`OUTCOME_RECORD`]'s line for `reopened` at `at` (unix seconds). Pure.
#[cfg(any(target_os = "macos", test))]
fn outcome_line(at: u64, reopened: Reopened) -> String {
    format!("{at} {} {}\n", reopened.word(), reopened.tries())
}

/// [`outcome_line`] read back: the seconds and the outcome; `None` for anything else. Pure.
#[cfg(any(target_os = "macos", test))]
fn read_outcome(text: &str) -> Option<(u64, Reopened)> {
    let mut words = text.strip_suffix('\n')?.split(' ');
    let (at, word, tries) = (words.next()?, words.next()?, words.next()?);
    let digits = |field: &str| !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit());
    if words.next().is_some() || !digits(at) || !digits(tries) {
        return None;
    }
    let tries: u32 = tries.parse().ok()?;
    let reopened = match word {
        "still-running" if tries == 0 => Reopened::StillRunning,
        "opened" if (1..=OPEN_TRIES).contains(&tries) => Reopened::Opened { tries },
        "open-failed" if tries == OPEN_TRIES => Reopened::OpenFailed,
        _ => return None,
    };
    Some((at.parse().ok()?, reopened))
}

/// The journal's line for the helper's `reopened` at `at`, read `now` (unix seconds, `None`
/// unread). Pure.
#[cfg(target_os = "macos")]
fn journal_line(at: u64, reopened: Reopened, now: Option<u64>) -> String {
    let ago = now.map_or_else(String::new, |now| {
        format!(", {} s ago", now.saturating_sub(at))
    });
    match reopened {
        Reopened::Opened { tries } => format!(
            "quit & reopen: the helper opened aterm again at `open` try {tries} of \
             {OPEN_TRIES}{ago}"
        ),
        Reopened::OpenFailed => format!(
            "quit & reopen: the helper's {OPEN_TRIES} `open` tries all failed{ago}; aterm was not \
             opened again"
        ),
        Reopened::StillRunning => format!(
            "quit & reopen: the quitting aterm still ran at the helper's ceiling{ago}; aterm was \
             not opened again"
        ),
    }
}

/// Leave `reopened` in `state`'s [`OUTCOME_RECORD`] at `at` (unix seconds), in place of what an
/// earlier helper left.
///
/// # Errors
/// The record could not be written (`config_marker::write_marker`).
#[cfg(any(target_os = "macos", test))]
fn note_outcome_in(state: &Path, at: u64, reopened: Reopened) -> std::io::Result<()> {
    crate::config_marker::write_marker(
        &state.join(OUTCOME_RECORD),
        &outcome_line(at, reopened),
        MAX_OUTCOME_BYTES,
        crate::durable::Flush::Ordered,
    )
}

/// Take what the last helper left in `state` ([`OUTCOME_RECORD`]): its seconds and outcome, the
/// record removed so no later aterm writes it again; `None` for no record, and for one that
/// does not read, which is removed too.
#[cfg(any(target_os = "macos", test))]
fn take_outcome_in(state: &Path) -> Option<(u64, Reopened)> {
    let path = state.join(OUTCOME_RECORD);
    let text = crate::config_marker::read_marker(&path, MAX_OUTCOME_BYTES)?;
    if let Err(error) = crate::config_marker::remove_marker(&path) {
        aterm_log::warn!("quit & reopen: the helper's record was not removed: {error}");
    }
    read_outcome(&text)
}

/// Write in this aterm's journal what the last Quit & Reopen helper did, from what it left in
/// `<state>`: the helper runs with no journal, so its `open`s, or its giving up, reach a journal
/// only here, at the next launch (the one it opened, when it did).
#[cfg(target_os = "macos")]
pub(crate) fn journal_last_reopen() {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return;
    };
    if let Some((at, reopened)) = take_outcome_in(&state) {
        aterm_log::info!("{}", journal_line(at, reopened, aterm_time::unix_secs()));
    }
}

/// The helper's course: once `gone` says the quitting aterm has exited, wait on `forgotten`
/// (LaunchServices dropping it, or that wait's ceiling), then `open` the app again, up to
/// [`OPEN_TRIES`] times, `pause` taking [`OPEN_RETRY`]'s pause before each (none before the
/// first).
#[cfg(any(target_os = "macos", test))]
fn reopen_once_gone(
    gone: impl FnOnce() -> bool,
    forgotten: impl FnOnce(),
    mut open: impl FnMut() -> bool,
    mut pause: impl FnMut(Duration),
) -> Reopened {
    if !gone() {
        return Reopened::StillRunning;
    }
    forgotten();
    for tries in 0..OPEN_TRIES {
        pause(OPEN_RETRY.after(tries));
        if open() {
            return Reopened::Opened { tries: tries + 1 };
        }
    }
    Reopened::OpenFailed
}

/// Leave the helper that opens aterm again once this process is gone, when this quit is a
/// Quit & Reopen ([`crate::system_quit::reopens`]) of an aterm inside a `.app`.
#[cfg(target_os = "macos")]
pub(crate) fn leave_reopen_at_quit() {
    use aterm_uds::spawnfd::Stdio;
    if !crate::system_quit::reopens() {
        return;
    }
    let Some(bundle) = aterm_containment::consent::fence_exe()
        .and_then(|exe| aterm_containment::consent::app_bundle_root(&exe))
    else {
        return;
    };
    let Some(target) = i32::try_from(std::process::id())
        .ok()
        .and_then(aterm_uds::process::read_identity)
    else {
        aterm_log::warn!("quit & reopen: cannot identify the quitting process");
        return;
    };
    let spawned = reopen_command(target, &bundle)
        .setsid(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(helper) => aterm_log::info!(
            "quit & reopen: pid {} opens {} once this process is gone",
            helper.id(),
            bundle.display()
        ),
        Err(error) => aterm_log::warn!(
            "quit & reopen: no helper to open {} again: {error}",
            bundle.display()
        ),
    }
}

/// [`REOPEN_VERB`]: wait for the named aterm process to be gone, then open the bundle named,
/// leaving what it did in `<state>` for the next aterm's journal ([`OUTCOME_RECORD`]).
#[must_use = "return or inspect the reopen helper's exit status"]
pub fn run_reopen_after(args: &[OsString]) -> std::process::ExitCode {
    #[cfg(target_os = "macos")]
    if let Some((target, bundle)) = parse_reopen_args(args) {
        let reopened = reopen_once_gone(
            || wait_gone(target, GONE_CEILING),
            || wait_forgotten(target.pid.unsigned_abs(), FORGOTTEN_CEILING),
            || open_bundle(&bundle),
            std::thread::sleep,
        );
        if let (Some(state), Some(at)) = (aterm_types::dirs::state_dir(), aterm_time::unix_secs()) {
            // Nothing reads this helper's output: a record it could not leave is lost with it.
            let _ = note_outcome_in(&state, at, reopened);
        }
        return if matches!(reopened, Reopened::Opened { .. }) {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        };
    }
    let _ = args;
    eprintln!("aterm {REOPEN_VERB}: a process token and an app bundle, on macOS");
    std::process::ExitCode::FAILURE
}

/// Whether `target` exited, was reaped or its pid was reused within `ceiling`.
#[cfg(target_os = "macos")]
fn wait_gone(target: Identity, ceiling: Duration) -> bool {
    use aterm_uds::process::{process_presence, read_birth};
    let end = Instant::now() + ceiling;
    let mut exit = aterm_uds::deadline::ExitWait::on_pid(target.pid.unsigned_abs());
    loop {
        if original_gone(
            target.birth,
            process_presence(target.pid),
            read_birth(target.pid),
        ) {
            return true;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        exit.wait(left);
    }
}

/// Track process lifetime, not exec: an unreadable birth is not evidence of exit.
#[cfg(any(target_os = "macos", test))]
fn original_gone(expected: Birth, presence: ProcessPresence, birth: Option<Birth>) -> bool {
    matches!(presence, ProcessPresence::Exited | ProcessPresence::Absent)
        || birth.is_some_and(|birth| birth != expected)
}

/// Wait, up to `ceiling`, until LaunchServices no longer lists `pid` as a running app.
#[cfg(target_os = "macos")]
fn wait_forgotten(pid: u32, ceiling: Duration) {
    let end = Instant::now() + ceiling;
    while launch_services_lists(pid) && Instant::now() < end {
        std::thread::sleep(FORGOTTEN_TICK);
    }
}

/// Whether LaunchServices lists `pid` as a running app.
#[cfg(target_os = "macos")]
fn launch_services_lists(pid: u32) -> bool {
    use aterm_objc::{Id, Sel, autoreleasepool, class, sel};
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    autoreleasepool(|_| {
        // SAFETY: `+runningApplicationWithProcessIdentifier:` is `+(instancetype)(pid_t)` on an
        // immortal AppKit class and answers nil or an autoreleased instance valid for this pool;
        // `-isTerminated` is a `BOOL` getter on that instance.
        unsafe {
            let with_pid: unsafe extern "C-unwind" fn(Id, Sel, i32) -> Id =
                aterm_objc::checked_msg();
            let app = with_pid(
                class(c"NSRunningApplication").as_id(),
                sel!(runningApplicationWithProcessIdentifier:),
                pid,
            );
            !app.is_null() && !crate::appkit::send_bool(app, sel!(isTerminated))
        }
    })
}

/// Ask LaunchServices to open `bundle` (`open -a`), in this helper's environment: the one the
/// quitting aterm had.
#[cfg(target_os = "macos")]
fn open_bundle(bundle: &Path) -> bool {
    let mut open = aterm_uds::spawnfd::command("/usr/bin/open");
    open.arg("-a").arg(bundle);
    aterm_uds::deadline::output_within(&mut open, OPEN_CEILING)
        .is_ok_and(|out| out.status.success())
}

#[cfg(test)]
#[path = "quit_reopen_tests.rs"]
mod tests;
