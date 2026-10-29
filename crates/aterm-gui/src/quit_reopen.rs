// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System Settings' Quit & Reopen. Once a person turns an access on or off for aterm (Full Disk
//! Access, say), System Settings asks aterm to quit, with a logout's quit reason (`rlgo`), waits
//! about three seconds, and reopens only an app that quit by then. aterm asks before it quits, so
//! it misses that wait. A quit System Settings asked for therefore leaves a helper behind that
//! opens aterm again once this process is gone.

use std::ffi::OsString;
#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};
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

/// The hidden verb `aterm __reopen-after <pid> <bundle>` ([`run_reopen_after`]).
pub const REOPEN_VERB: &str = "__reopen-after";

/// How long the helper waits for the quitting aterm to be gone before it gives up.
const GONE_CEILING: Duration = Duration::from_secs(120);

/// How long the helper waits for LaunchServices to drop the gone aterm before it opens anyway:
/// LaunchServices answers an `open` for an app it still lists by bringing that app forward.
const FORGOTTEN_CEILING: Duration = Duration::from_secs(5);

#[cfg(target_os = "macos")]
const FORGOTTEN_TICK: Duration = Duration::from_millis(20);

const OPEN_CEILING: Duration = Duration::from_secs(60);

/// The longest the helper takes, from its start at this process's quit, to open aterm again: its
/// waits for the quitting aterm to be gone, for LaunchServices to drop it, and for `open`.
pub(crate) const REOPEN_CEILING: Duration = Duration::from_secs(
    GONE_CEILING.as_secs() + FORGOTTEN_CEILING.as_secs() + OPEN_CEILING.as_secs(),
);

/// Whether a quit sent by the program at `path` is System Settings' Quit & Reopen.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn sender_reopens(path: &Path) -> bool {
    REOPEN_SENDERS
        .iter()
        .any(|sender| path == Path::new(sender))
}

/// The arguments of the helper that opens `bundle` again once `pid` is gone.
#[cfg(any(target_os = "macos", test))]
fn reopen_args(pid: u32, bundle: &Path) -> [OsString; 3] {
    [
        REOPEN_VERB.into(),
        pid.to_string().into(),
        bundle.as_os_str().to_owned(),
    ]
}

/// The helper's command: the bundle's front door, `Contents/MacOS/aterm`, with [`reopen_args`].
/// Not this process's own path: an aterm started under a window alias (`Contents/MacOS/aterm-gui`)
/// would start the helper under that name too, and the router hands a window alias's arguments
/// to a window, so the verb would never be read.
#[cfg(any(target_os = "macos", test))]
fn reopen_command(pid: u32, bundle: &Path) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(bundle.join("Contents/MacOS/aterm"));
    command.args(reopen_args(pid, bundle));
    command
}

/// [`reopen_args`] read back past the verb: a pid above 1 and an absolute path to a `.app`.
#[cfg(any(target_os = "macos", test))]
fn parse_reopen_args(args: &[OsString]) -> Option<(u32, PathBuf)> {
    let [pid, bundle] = args else {
        return None;
    };
    let pid = pid
        .to_str()?
        .parse::<i32>()
        .ok()
        .filter(|pid| *pid > 1)
        .and_then(|pid| u32::try_from(pid).ok())?;
    let bundle = PathBuf::from(bundle);
    let app = bundle
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"));
    (bundle.is_absolute() && app).then_some((pid, bundle))
}

/// What the helper did.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reopened {
    /// The quitting aterm still ran at the ceiling: nothing was opened.
    StillRunning,
    /// `open` opened the app.
    Opened,
    /// `open` failed.
    OpenFailed,
}

/// The helper's course: once `gone` says the quitting aterm has exited, wait on `forgotten`
/// (LaunchServices dropping it, or that wait's ceiling), then `open` the app again.
#[cfg(any(target_os = "macos", test))]
fn reopen_once_gone(
    gone: impl FnOnce() -> bool,
    forgotten: impl FnOnce(),
    open: impl FnOnce() -> bool,
) -> Reopened {
    if !gone() {
        return Reopened::StillRunning;
    }
    forgotten();
    if open() {
        Reopened::Opened
    } else {
        Reopened::OpenFailed
    }
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
    let spawned = reopen_command(std::process::id(), &bundle)
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

/// [`REOPEN_VERB`]: wait for the aterm at the pid named to be gone, then open the bundle named.
#[must_use]
pub fn run_reopen_after(args: &[OsString]) -> std::process::ExitCode {
    #[cfg(target_os = "macos")]
    if let Some((pid, bundle)) = parse_reopen_args(args) {
        let reopened = reopen_once_gone(
            || wait_gone(pid, GONE_CEILING),
            || wait_forgotten(pid, FORGOTTEN_CEILING),
            || open_bundle(&bundle),
        );
        return if reopened == Reopened::Opened {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        };
    }
    let _ = args;
    eprintln!("aterm {REOPEN_VERB}: a pid and an app bundle, on macOS");
    std::process::ExitCode::FAILURE
}

/// Whether `pid` exited (or was reaped) within `ceiling`, as the kernel reports it.
#[cfg(target_os = "macos")]
fn wait_gone(pid: u32, ceiling: Duration) -> bool {
    use aterm_uds::process::{ProcessPresence, process_presence};
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    let end = Instant::now() + ceiling;
    let mut exit = aterm_uds::deadline::ExitWait::on_pid(pid);
    loop {
        if matches!(
            process_presence(raw),
            ProcessPresence::Exited | ProcessPresence::Absent
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
mod tests {
    use super::*;

    #[test]
    fn only_system_settings_quit_is_a_quit_and_reopen() {
        for sender in REOPEN_SENDERS {
            assert!(sender_reopens(Path::new(sender)), "{sender}");
        }
        assert_eq!(
            REOPEN_SENDERS[0],
            "/System/Library/ExtensionKit/Extensions/SecurityPrivacyExtension.appex/Contents/\
             MacOS/SecurityPrivacyExtension"
        );
        for other in [
            "/System/Library/CoreServices/Dock.app/Contents/MacOS/Dock",
            "/usr/bin/osascript",
            "/System/Library/ExtensionKit/Extensions/SecurityPrivacyExtension.appex",
            "/Applications/System Settings.app/Contents/MacOS/System Settings",
            "/tmp/System/Applications/System Settings.app/Contents/MacOS/System Settings",
            "System/Applications/System Settings.app/Contents/MacOS/System Settings",
            "",
        ] {
            assert!(!sender_reopens(Path::new(other)), "{other}");
        }
        assert_eq!(KEY_SENDER_PID, 0x7370_6964);
    }

    /// An absolute path to a `.app` on the host the test runs on.
    fn host_bundle() -> &'static Path {
        Path::new(if cfg!(windows) {
            r"C:\Applications\aterm.app"
        } else {
            "/Applications/aterm.app"
        })
    }

    #[test]
    fn the_helper_reads_back_only_what_the_quit_wrote() {
        let bundle = host_bundle();
        let args = reopen_args(4663, bundle);
        assert_eq!(args[0], REOPEN_VERB);
        assert_eq!(
            parse_reopen_args(&args[1..]),
            Some((4663, bundle.to_path_buf()))
        );
        let app = bundle.to_str().expect("a UTF-8 path");
        let exe = bundle.join("Contents/MacOS/aterm");
        let exe = exe.to_str().expect("a UTF-8 path");
        let refused: [&[&str]; 9] = [
            &[],
            &["4663"],
            &["4663", app, "more"],
            &["0", app],
            &["1", app],
            &["-4663", app],
            &["2147483648", app],
            &["4663", "Applications/aterm.app"],
            &["4663", exe],
        ];
        for words in refused {
            let args: Vec<OsString> = words.iter().map(OsString::from).collect();
            assert_eq!(parse_reopen_args(&args), None, "{words:?}");
        }
    }

    #[test]
    fn the_helper_runs_the_bundles_front_door_whatever_name_this_aterm_ran_under() {
        let bundle = host_bundle();
        let command = reopen_command(4663, bundle);
        let program = Path::new(command.get_program());
        assert_eq!(program, bundle.join("Contents/MacOS/aterm"));
        assert_eq!(program.file_name(), Some(std::ffi::OsStr::new("aterm")));
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        let wrote = reopen_args(4663, bundle);
        assert_eq!(
            args,
            wrote.iter().map(OsString::as_os_str).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_helper_opens_only_once_the_quitting_aterm_is_gone() {
        use std::cell::RefCell;
        let steps = RefCell::new(Vec::new());
        let run = |gone: bool, opened: bool| {
            steps.borrow_mut().clear();
            let reopened = reopen_once_gone(
                || {
                    steps.borrow_mut().push("gone");
                    gone
                },
                || steps.borrow_mut().push("forgotten"),
                || {
                    steps.borrow_mut().push("open");
                    opened
                },
            );
            (reopened, steps.borrow().clone())
        };
        assert_eq!(run(false, true), (Reopened::StillRunning, vec!["gone"]));
        assert_eq!(
            run(true, true),
            (Reopened::Opened, vec!["gone", "forgotten", "open"])
        );
        assert_eq!(
            run(true, false),
            (Reopened::OpenFailed, vec!["gone", "forgotten", "open"])
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_wait_ends_at_the_exit_and_not_before() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let mut child = aterm_uds::spawnfd::command(&sleep)
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        assert!(
            !wait_gone(pid, Duration::from_millis(200)),
            "a running process read as gone"
        );
        child.kill().expect("kill sleep");
        assert!(
            wait_gone(pid, Duration::from_secs(60)),
            "the exit of an unreaped child was missed"
        );
        let _ = child.wait();
    }
}
