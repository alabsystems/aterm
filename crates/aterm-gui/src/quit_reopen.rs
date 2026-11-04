// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System Settings' Quit & Reopen. Once a person turns an access on or off for aterm (Full Disk
//! Access, say), System Settings asks aterm to quit, with a logout's quit reason (`rlgo`), waits
//! about three seconds, and reopens only an app that quit by then. aterm asks before it quits, so
//! it misses that wait. A quit System Settings asked for therefore leaves a helper behind that
//! opens aterm again once this process is gone.

#[cfg(any(target_os = "macos", test))]
use aterm_uds::process::{Birth, Identity, ProcessPresence};
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

/// The hidden verb `aterm __reopen-after <process-token> <bundle>` ([`run_reopen_after`]).
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

/// [`REOPEN_VERB`]: wait for the named aterm process to be gone, then open the bundle named.
#[must_use = "return or inspect the reopen helper's exit status"]
pub fn run_reopen_after(args: &[OsString]) -> std::process::ExitCode {
    #[cfg(target_os = "macos")]
    if let Some((target, bundle)) = parse_reopen_args(args) {
        let reopened = reopen_once_gone(
            || wait_gone(target, GONE_CEILING),
            || wait_forgotten(target.pid.unsigned_abs(), FORGOTTEN_CEILING),
            || open_bundle(&bundle),
        );
        return if reopened == Reopened::Opened {
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
mod tests {
    use super::*;

    fn target() -> Identity {
        Identity {
            pid: 4663,
            birth: Birth(123),
            idversion: aterm_uds::process::KEEPS_PID_VERSIONS.then_some(7),
        }
    }

    #[test]
    fn a_reused_pid_is_gone_but_an_unreadable_birth_is_not() {
        let expected = Birth(123);
        assert!(original_gone(
            expected,
            ProcessPresence::Present,
            Some(Birth(124))
        ));
        assert!(!original_gone(
            expected,
            ProcessPresence::Present,
            Some(expected)
        ));
        for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
            assert!(!original_gone(expected, presence, None));
        }
    }

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
        let target = target();
        let args = reopen_args(target, bundle);
        assert_eq!(args[0], REOPEN_VERB);
        assert_eq!(
            parse_reopen_args(&args[1..]),
            Some((target, bundle.to_path_buf()))
        );
        let app = bundle.to_str().expect("a UTF-8 path");
        let exe = bundle.join("Contents/MacOS/aterm");
        let exe = exe.to_str().expect("a UTF-8 path");
        let token = target.token();
        let token = token.as_str();
        let refused: [&[&str]; 6] = [
            &[],
            &[token],
            &[token, app, "more"],
            &["4663", app],
            &[token, "Applications/aterm.app"],
            &[token, exe],
        ];
        for words in refused {
            let args: Vec<OsString> = words.iter().map(OsString::from).collect();
            assert_eq!(parse_reopen_args(&args), None, "{words:?}");
        }
    }

    #[test]
    fn the_helper_runs_the_bundles_front_door_whatever_name_this_aterm_ran_under() {
        let bundle = host_bundle();
        let target = target();
        let command = reopen_command(target, bundle);
        let program = Path::new(command.get_program());
        assert_eq!(program, bundle.join("Contents/MacOS/aterm"));
        assert_eq!(program.file_name(), Some(std::ffi::OsStr::new("aterm")));
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        let wrote = reopen_args(target, bundle);
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
        let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
        let mut child = aterm_uds::spawnfd::command(&cat)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .expect("spawn cat with held input");
        let target = i32::try_from(child.id())
            .ok()
            .and_then(aterm_uds::process::read_identity)
            .expect("the child's exact identity");
        assert!(
            !wait_gone(target, Duration::ZERO),
            "a running process read as gone"
        );
        child.kill().expect("kill cat");
        assert!(
            wait_gone(target, Duration::from_secs(60)),
            "the exit of an unreaped child was missed"
        );
        aterm_uds::deadline::kill_within(&mut child, Duration::from_secs(60));
    }
}
