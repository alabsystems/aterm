// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Start an application bundle as its own launchd application job.
//!
//! On macOS the outgoing process is the process of the launchd job
//! `application.com.aterm.aterm.<hex>.<hex>` LaunchServices minted for this instance. When it
//! `_exit`s in `seamless::commit_and_exit`, launchd tears the job down, and a fork child is left
//! with a bootstrap (XPC) context for a job that no longer exists: `hdiutil` then answers `ENXIO`,
//! so the survivor can never apply the next update (`tests/handoff_launchd_job.rs` guards this).
//! A LaunchServices launch gets a job, and a bootstrap domain, of its own (it is born with
//! `ppid == 1`). This is the launcher of `app_update_handoff`'s out-of-band lane; the fork lane
//! is the fallback.
//!
//! The seamless-update handoff currently forks its successor
//! (`std::process::Command::spawn` in `app_update_handoff::run_handoff_worker`),
//! and on macOS that is a latent defect with a silent symptom. The outgoing
//! process IS the process of the launchd job
//! `application.com.aterm.aterm.<hex>.<hex>` that LaunchServices minted for this
//! app instance; when it `_exit(0)`s inside `seamless::commit_and_exit`, launchd
//! tears that job down and the fork child keeps running with a bootstrap (XPC)
//! context belonging to a job that no longer exists. Nothing looks wrong — the
//! shell survived, the window is there — until the survivor asks for something
//! that needs the app's own XPC domain: `hdiutil` answers `ENXIO`, so the
//! process that just applied an update can never apply the next one.
//!
//! It launches and reports, nothing more.
//!
//! * It transfers no descriptors: a LaunchServices launch inherits none, so the PTY masters and
//!   the readiness/Commit pipes travel over the rendezvous socket (`SCM_RIGHTS`).
//! * The completion answer is not liveness: it says LaunchServices accepted or refused the
//!   launch, not that a successor runs or is ready. The rendezvous dial is the liveness signal.
//! * It never exits, signals or kills, and never touches the outgoing process's sessions. Every
//!   failure is a typed [`LaunchError`]; the caller falls back to the fork lane or rolls back.
//!   A refusal before the AppKit call leaves the machine untouched.
//!
//! # The environment merges
//!
//! Keys set on `NSWorkspaceOpenConfiguration.environment` arrive merged over the launching
//! process's own environment (`PATH`, `HOME`, the session's `XPC_*`), so pass only the handoff
//! keys: reconstructing an environment would mean keeping `XPC_SERVICE_NAME` alive by hand.
//!
//! Never pass a key whose value is a descriptor number (`ATERM_HANDOFF_READY_FD`,
//! `ATERM_HANDOFF_COMMIT_FD`, the fd column of `ATERM_SEAMLESS_FDS`): those name entries in a
//! fork child's table, and in a launched process the same integers name whatever LaunchServices
//! left there. This module cannot tell a descriptor number from other digits, so the rule is
//! the caller's.
//!
//! # A timeout is not a refusal
//!
//! `openApplicationAtURL:configuration:completionHandler:` is asynchronous: [`begin_launch`]
//! returns a [`LaunchInFlight`] whose `wait(budget)` blocks for the answer. The budget bounds how
//! long the caller waits, never how long LaunchServices takes, so `LaunchError::Timeout` means
//! "no answer yet", not "no successor": a process may still dial the rendezvous. The caller
//! treats the dial as proof a successor exists and keeps its rollback idempotent against a late
//! one (the socket sweep in `seamless::discard_outgoing` makes a late dial fail closed). An early
//! `Err` is still worth reading: it is the earliest negative answer.
//!
//! # Threading
//!
//! This blocks, so it must NOT run on the main thread — a blocked main thread is
//! a frozen terminal, and it would deadlock outright if AppKit ever delivered
//! the completion to the main queue. It refuses there
//! (`LaunchError::WouldBlockMainThread`) rather than trusting the caller.
//!
//! No main-thread hop is needed to make the call itself. `NSWorkspace` and
//! `NSRunningApplication` are not main-thread-only: objc2 models both as
//! `Mutability = InteriorMutable` (derived from Apple's own main-actor
//! annotations) where `NSWindow` and `NSAlert` — which genuinely are — come out
//! as `MainThreadOnly`. That is also why this module reads the completion's
//! `processIdentifier` off the main thread, and why it does NOT follow
//! `menu.rs::open_in_workspace`, which gates on `MainThreadMarker` because the
//! synchronous `openURL:` it calls goes on to touch the frontmost-app machinery.
//!
//! Objects are held as `Retained` (+1) exactly as `menu.rs` / `toolbar.rs` hold
//! theirs, and the transient autoreleased ones this creates are drained by the
//! pool the caller's thread already runs under; there is no explicit
//! `autoreleasepool` here for the same reason there is none anywhere else in
//! this crate.
//!
//! # Build surface
//!
//! **There is no build surface left to describe, and that is the W13 change.**
//! This section used to say that the typed `NSWorkspace` bindings needed
//! `objc2-app-kit`'s `NSWorkspace` feature enabled, and that `menu.rs` and
//! `platform.rs` reached NSWorkspace through `class!` + `msg_send!` *precisely
//! because that feature was off* — a real cost, paid in untyped sends, to avoid
//! widening a third-party feature list.
//!
//! That trade no longer exists in either direction. Every call here goes
//! through `aterm_objc` — `class(c"NSWorkspace")`, the
//! `NSWorkspaceOpenConfiguration` class, `aterm_objc::checked_msg()` for each
//! prototype and `RcBlock` for the completion handler — and `aterm-gui`'s
//! four `objc2` family rows were retired in the same wave. A binding is now
//! reachable because it was written, not because a feature flag was set, so
//! the untyped/typed choice is made per call site on its merits: this one
//! takes a URL, a configuration object, a dictionary and a block whose
//! signature must match what AppKit invokes, and it declares every one of
//! those prototypes explicitly.

use std::ffi::OsString;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

/// A successfully launched successor.
///
/// The pid is diagnostic, not authority: the successor is launchd's child, so it cannot be
/// `wait`ed on, and a pid alone is reusable. The handoff acts on the identity the kernel attests
/// with `LOCAL_PEERPID` at the rendezvous `accept`, cross-checked against the birth record
/// `seamless::AttestedParent` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaunchedSuccessor {
    pid: i32,
}

impl LaunchedSuccessor {
    /// The launched instance's pid, as LaunchServices reported it: positive and never this
    /// process's own (`admit_launched_pid`, the only non-test constructor, refuses both).
    pub(crate) fn pid(self) -> i32 {
        self.pid
    }
}

/// Why a piece of text cannot be carried to LaunchServices. `arguments` and `environment` ride
/// `NSString`s: non-UTF-8 has no representation, and an interior NUL would be truncated by every
/// C-string consumer after `execve`. A refused launch falls back to the fork lane; a corrupted
/// handoff key would not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextFault {
    NotUtf8,
    InteriorNul,
    Empty,
    /// An `=`, which `environ` uses to separate a key from its value.
    Separator,
}

impl std::fmt::Display for TextFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotUtf8 => f.write_str("is not UTF-8"),
            Self::InteriorNul => f.write_str("contains a NUL byte"),
            Self::Empty => f.write_str("is empty"),
            Self::Separator => f.write_str("contains '='"),
        }
    }
}

/// Every way a launch ([`begin_launch`], [`LaunchInFlight::wait`]) can fail to produce a
/// successor; each is the caller's cue to keep the user's windows and fall back. Split finely so
/// "not on disk" and "LaunchServices refused" get different remedies and a wiring mistake (a
/// duplicated key, a relative path) cannot hide inside an OS failure. Each platform-specific
/// variant is `cfg`'d to the platform that can produce it, so matching the wrong one is a compile
/// error; the request refusals are platform-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchError {
    /// Not macOS. There is no launchd application job to mint, so there is
    /// nothing this lane can honestly do.
    #[cfg(not(target_os = "macos"))]
    Unsupported,
    /// The bundle path itself cannot be expressed as an `NSString`.
    BundlePath {
        path: String,
        fault: TextFault,
    },
    /// LaunchServices resolves nothing relative to a working directory, and the
    /// caller's working directory is not the successor's anyway.
    BundlePathNotAbsolute(String),
    /// Only a bundle gets an `application.<bundle-id>.<hex>` job of its own,
    /// which is the entire point of this lane; refusing early turns a wrong
    /// target into a typed refusal instead of an opaque LaunchServices status
    /// a whole deadline later.
    BundlePathNotAnAppBundle(String),
    /// Nothing is at that path (or it is not a directory — every `.app` is one).
    /// Advisory only: it races the filesystem, and the authoritative answer is
    /// still the completion handler's.
    BundleMissing(String),
    Argument {
        index: usize,
        fault: TextFault,
    },
    EnvKey {
        index: usize,
        fault: TextFault,
    },
    EnvValue {
        key: String,
        fault: TextFault,
    },
    /// The same key twice. An `NSDictionary` would silently keep one, so the successor's value
    /// would depend on argument order.
    EnvKeyDuplicated(String),
    /// Called on the main thread, where blocking would freeze the terminal.
    #[cfg(target_os = "macos")]
    WouldBlockMainThread,
    /// The budget expired with no answer. Not proof that nothing was launched (module docs).
    #[cfg(target_os = "macos")]
    Timeout(Duration),
    /// LaunchServices answered with an `NSError`.
    #[cfg(target_os = "macos")]
    Rejected {
        domain: String,
        code: isize,
        message: String,
    },
    /// The completion handler fired with neither an application nor an error.
    /// Documented not to happen (success carries a running application and a
    /// null error); treated as a failure rather than assumed away.
    #[cfg(target_os = "macos")]
    NoApplication,
    /// Foundation refused to mint one of the objects the launch needs. Only an allocation
    /// failure gets here: every string has already passed validation.
    #[cfg(target_os = "macos")]
    ForeignObject(&'static str),
    /// LaunchServices reported an implausible pid — `NSRunningApplication`
    /// answers `-1` when it does not know one.
    ///
    /// AN `execve` AFTER THE LAUNCH IS ONE CAUSE (measured 2026-10-19 on macOS 27,
    /// on the owner's Mac and with scratch stand-in apps): a process that replaces its
    /// image some 240–400 ms after LaunchServices launched it is answered `-1` and
    /// not terminated, `lsappinfo` lists it as `[ NULL ]`, and the record stays stale
    /// for the process's life (System Settings' Quit & Reopen quits by pid and fails
    /// with -600, the menu-bar item is refused for a "mismatched pid version", and
    /// `tccd` attributes it to the bundle it first saw). An exec at 0 ms kept its pid,
    /// so it is a race and the 2026-09-29 count below did not see it. No successor this
    /// build launches through LaunchServices execs any more: its parent swaps before it
    /// launches. (A fork-lane child still swaps and re-execs, but LaunchServices never
    /// launched it.)
    #[cfg(target_os = "macos")]
    PidUnknown(i32),
    /// LaunchServices answered with an application that had ALREADY TERMINATED
    /// and whose pid it no longer knows (`-1`): the launched process ran and
    /// exited before it ever checked in (2026-09-29, measured on macOS 26.6.2
    /// against an ad-hoc-signed stand-in bundle launched exactly as
    /// [`begin_launch`] launches). A process that dies before it creates its
    /// `NSApplication` — for a successor, before its rendezvous dial — was
    /// answered this way 26–251 ms after its death (median 145 ms), however
    /// long it had lived: 23 of 23 runs — an immediate `_exit`, `_exit` after
    /// 0.5, 2 and 8 s, `SIGKILL` after 0.3 and 1 s, and, after a same-pid
    /// re-exec, an immediate `_exit`, `_exit` after 1 s and `SIGKILL` after
    /// 0.3 s. A process that is ALIVE was never answered this way:
    /// the 8 that checked in (3 after a re-exec, at an exec delay not recorded;
    /// see [`Self::PidUnknown`] for the later exec that loses the pid) were answered
    /// with their pid when they did, and the 2 that never did were answered with their pid, not
    /// terminated, at LaunchServices' own check-in timeout (30.06 s, 30.08 s). So
    /// this one answer is POSITIVE EVIDENCE OF DEATH, the only one this module
    /// produces ([`launch_answer_proves_exit`]). A launch refused before any
    /// process exists (a missing or non-executable binary) is answered with an
    /// `NSError` instead, which is never read as death. After a dial it
    /// contradicts any dialer that is still alive, which cannot be the dead
    /// process (`app_update_handoff::LaunchCorroboration`).
    #[cfg(target_os = "macos")]
    ExitedBeforeCheckIn,
    /// The answer names this process: LaunchServices substituted the running instance, so no
    /// successor exists. Not expected ([`begin_launch`] forbids substitution) and never driven.
    /// Nothing refuses on it: the dial wait reads it as naming no process and waits out its
    /// budget (`app_update_handoff::await_launched_dial`), and after a dial the corroboration
    /// proceeds on the attested dialer, as for every error but [`Self::ExitedBeforeCheckIn`].
    #[cfg(target_os = "macos")]
    SubstitutedRunningInstance(i32),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(not(target_os = "macos"))]
            Self::Unsupported => {
                f.write_str("launching an application bundle as its own launchd job is macOS-only")
            }
            Self::BundlePath { path, fault } => {
                write!(f, "the application bundle path {path} {fault}")
            }
            Self::BundlePathNotAbsolute(path) => {
                write!(f, "the application bundle path {path} is not absolute")
            }
            Self::BundlePathNotAnAppBundle(path) => write!(f, "{path} is not a .app bundle"),
            Self::BundleMissing(path) => write!(f, "{path} is not a directory on this machine"),
            Self::Argument { index, fault } => write!(f, "launch argument #{index} {fault}"),
            Self::EnvKey { index, fault } => write!(f, "launch environment key #{index} {fault}"),
            Self::EnvValue { key, fault } => {
                write!(f, "the value of launch environment key {key} {fault}")
            }
            Self::EnvKeyDuplicated(key) => {
                write!(f, "launch environment key {key} was given more than once")
            }
            #[cfg(target_os = "macos")]
            Self::WouldBlockMainThread => {
                f.write_str("a blocking launch was attempted on the main thread")
            }
            #[cfg(target_os = "macos")]
            Self::Timeout(budget) => write!(f, "LaunchServices did not answer within {budget:?}"),
            #[cfg(target_os = "macos")]
            Self::Rejected {
                domain,
                code,
                message,
            } => write!(
                f,
                "LaunchServices refused the launch: {message} ({domain} {code})"
            ),
            #[cfg(target_os = "macos")]
            Self::NoApplication => {
                f.write_str("LaunchServices reported neither an application nor an error")
            }
            #[cfg(target_os = "macos")]
            Self::ForeignObject(what) => {
                write!(f, "Foundation refused to build the launch's {what}")
            }
            #[cfg(target_os = "macos")]
            Self::PidUnknown(pid) => {
                write!(
                    f,
                    "LaunchServices reported the launched instance as pid {pid}"
                )
            }
            #[cfg(target_os = "macos")]
            Self::ExitedBeforeCheckIn => f.write_str(
                "LaunchServices reports the launched application terminated before it checked in",
            ),
            #[cfg(target_os = "macos")]
            Self::SubstitutedRunningInstance(pid) => write!(
                f,
                "LaunchServices answered with the already-running instance (pid {pid}) \
                 instead of starting a new one"
            ),
        }
    }
}

/// A launch request every field of which LaunchServices can express: valid UTF-8, no interior
/// NUL, no duplicate environment key. Validation is separate from the AppKit call so the
/// refusals (each a fallback to the fork lane) are pure, unit-testable and identical on every
/// platform.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchRequest {
    bundle: String,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
}

/// [`begin_launch`] then [`LaunchInFlight::wait`] for at most `budget`: the off-macOS test's pin
/// of the validate-then-refuse contract.
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) fn launch_app_bundle(
    bundle: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    budget: Duration,
) -> Result<LaunchedSuccessor, LaunchError> {
    // Validated on every platform ahead of the platform check, so a wiring mistake fails the
    // same way everywhere; the non-macOS stub then refuses the platform, not the request.
    let request = validate_request(bundle, arguments, environment)?;
    begin_launch_validated(&request, false)?.wait(budget)
}

/// Validate the request and hand it to LaunchServices, returning the in-flight handle at once so
/// the caller can accept the successor's rendezvous dial while the answer is pending. The
/// successor dials before any `NSApplication` exists and LaunchServices answers only once the app
/// checks in, so the dial always precedes the answer: waiting for the answer first would freeze
/// every launched-lane handoff for the whole answer budget. The answer was never the liveness
/// signal; the dial is.
///
/// `arguments` become the successor's `argv[1..]`; `environment` is merged over the launching
/// process's own, so pass only the handoff keys and never a descriptor number (module docs).
/// `activate` is whether LaunchServices brings the successor to the front; the handoff passes
/// `false` and activates at Commit ([`activate_running`]).
pub(crate) fn begin_launch(
    bundle: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    activate: bool,
) -> Result<LaunchInFlight, LaunchError> {
    let request = validate_request(bundle, arguments, environment)?;
    begin_launch_validated(&request, activate)
}

/// What LaunchServices' list of this app says about this process
/// ([`own_launch_record`]).
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnLaunchRecord {
    /// An entry carries this process's pid: Quit & Reopen, the menu bar and TCC find it.
    Named,
    /// No entry carries it and one carries none (`-1`): the stale record an `execve`
    /// after a LaunchServices launch leaves ([`LaunchError::PidUnknown`]), which lasts
    /// the process's life, and which a launch-record renewal replaces with a fresh
    /// process (`launch_record_renewal`).
    Stale,
    /// Neither: no bundle identifier, or no entry without a pid to be this one.
    Unknown,
}

/// How long into a windowed launch [`own_launch_record`] is asked: the stale record
/// formed within about a second of the `execve` in every measured run (2026-10-19),
/// and the launch's own exec comes before the window.
#[cfg(target_os = "macos")]
pub(crate) const OWN_LAUNCH_RECORD_SETTLE: std::time::Duration = std::time::Duration::from_secs(10);

/// Ask LaunchServices for every running instance of this bundle
/// (`+[NSRunningApplication runningApplicationsWithBundleIdentifier:]`) and whether one
/// carries this process's pid. The process's own measure of what
/// [`LaunchError::PidUnknown`] infers from its parent's launch: it also sees a successor
/// an older build launched, which swapped and re-exec'd itself, and a cold launch whose
/// boot apply did (a window does only at the left-stage escape with its automatic lane
/// on, after [`aterm_update::LEFT_STAGE_ESCAPE`] launches that left the stage ended or
/// when the record of them cannot count it, or to revert a crash loop). Any thread
/// (read-only queries, under their own pool); reads the bundle's `Info.plist` once.
#[cfg(target_os = "macos")]
pub(crate) fn own_launch_record() -> OwnLaunchRecord {
    use aterm_objc::{autoreleasepool, class, sel};
    let Some(bundle_id) = crate::signing_identity::running_bundle_id() else {
        return OwnLaunchRecord::Unknown;
    };
    let own = i32::try_from(std::process::id()).unwrap_or(-1);
    autoreleasepool(|_| {
        let Some(id) = crate::appkit::nsstring(bundle_id) else {
            return OwnLaunchRecord::Unknown;
        };
        // SAFETY: `+runningApplicationsWithBundleIdentifier:` is `+(NSArray *)
        // (NSString *)`, +0, valid for this pool; `-count` is `-(NSUInteger)`,
        // `-objectAtIndex:` `-(id)(NSUInteger)` below the count, and
        // `-processIdentifier` `-(pid_t)` on each element.
        let pids = unsafe {
            let apps = crate::appkit::send_id_id(
                class(c"NSRunningApplication").as_id(),
                sel!(runningApplicationsWithBundleIdentifier:),
                id.id(),
            );
            if apps.is_null() {
                return OwnLaunchRecord::Unknown;
            }
            (0..crate::appkit::send_usize(apps, sel!(count)))
                .map(|i| crate::appkit::send_id_usize(apps, sel!(objectAtIndex:), i))
                .filter(|app| !app.is_null())
                .map(|app| crate::appkit::send_i32(app, sel!(processIdentifier)))
                .collect::<Vec<_>>()
        };
        if pids.contains(&own) {
            OwnLaunchRecord::Named
        } else if pids.contains(&-1) {
            OwnLaunchRecord::Stale
        } else {
            OwnLaunchRecord::Unknown
        }
    })
}

/// A launch LaunchServices has been asked for, whose answer may still be in
/// flight. [`Self::wait`] blocks for it within a budget; a caller that has
/// already met the successor another way (its rendezvous dial) polls with a
/// short budget purely to corroborate the pid.
/// Bring the successor to the front at COMMIT (2026-09-19). The launch itself
/// never activates (`begin_launch(.., false)`): activation at launch time pulled
/// focus while the outgoing window was still the one on glass — a live screen
/// with a dead keyboard — and under the late park the successor exists for as
/// long as the parent waits for a quiet moment. So at Commit, immediately before
/// its own `_exit`, the parent reads whether an aterm window has focus and, if
/// one has, asks AppKit to activate the successor: `NSRunningApplication
/// runningApplicationWithProcessIdentifier:`, then `-[NSApp
/// yieldActivationToApplication:]` where AppKit answers to it (macOS 14+, the
/// cooperative half — an app may only take activation the front app yields),
/// then `-[NSRunningApplication activateWithOptions:]` ignoring other apps.
/// `true` when AppKit accepted the request. Refuses `pid <= 1`. Main-thread
/// callable, unlike `begin_launch`: Commit runs on the main thread.
#[cfg(target_os = "macos")]
pub(crate) fn activate_running(pid: i32) -> bool {
    use aterm_objc::{Bool, Id, Sel, autoreleasepool, class, sel};
    if pid <= 1 {
        return false;
    }
    // Only a windowed process yields activation: headless, the
    // `+sharedApplication` below would CREATE the application object.
    if crate::platform::Windowed::get().is_none() {
        return false;
    }
    /// `NSApplicationActivateIgnoringOtherApps` (AppKit, `NSApplicationActivationOptions`).
    const ACTIVATE_IGNORING_OTHER_APPS: usize = 1 << 1;
    autoreleasepool(|_| {
        // SAFETY: class methods on immortal AppKit classes and instance methods
        // on live objects: `runningApplicationWithProcessIdentifier:` answers nil
        // or an autoreleased instance valid for this pool; `sharedApplication`
        // is this process's own NSApplication (Commit runs on the main thread,
        // where it exists); `respondsToSelector:` is a side-effect-free query;
        // `yieldActivationToApplication:` and `activateWithOptions:` take the
        // instance and a bitmask by value.
        unsafe {
            let running_with_pid: unsafe extern "C-unwind" fn(Id, Sel, i32) -> Id =
                aterm_objc::checked_msg();
            let successor = running_with_pid(
                class(c"NSRunningApplication").as_id(),
                sel!(runningApplicationWithProcessIdentifier:),
                pid,
            );
            if successor.is_null() {
                return false;
            }
            let shared: unsafe extern "C-unwind" fn(Id, Sel) -> Id = aterm_objc::checked_msg();
            let nsapp = shared(class(c"NSApplication").as_id(), sel!(sharedApplication));
            if !nsapp.is_null() {
                let responds: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Bool =
                    aterm_objc::checked_msg();
                if responds(
                    nsapp,
                    sel!(respondsToSelector:),
                    sel!(yieldActivationToApplication:),
                )
                .as_bool()
                {
                    let yield_to: unsafe extern "C-unwind" fn(Id, Sel, Id) =
                        aterm_objc::checked_msg();
                    yield_to(nsapp, sel!(yieldActivationToApplication:), successor);
                }
            }
            let activate: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Bool =
                aterm_objc::checked_msg();
            activate(
                successor,
                sel!(activateWithOptions:),
                ACTIVATE_IGNORING_OTHER_APPS,
            )
            .as_bool()
        }
    })
}

/// Off macOS there is no LaunchServices successor to activate.
#[cfg(not(target_os = "macos"))]
pub(crate) fn activate_running(pid: i32) -> bool {
    let _ = pid;
    false
}

/// A launch LaunchServices has been asked for, whose answer may still be in flight.
/// [`Self::wait`] blocks for it within a budget; a caller that already met the successor (its
/// rendezvous dial) polls with a short budget to corroborate the pid.
pub(crate) struct LaunchInFlight {
    #[cfg(target_os = "macos")]
    watch: LaunchWatch,
}

impl LaunchInFlight {
    /// Block until the launch is answered or `budget` is spent. A zero budget is a poll: an
    /// answer already delivered, else `Timeout(ZERO)` at once, so corroboration never holds a
    /// parked terminal.
    ///
    /// * `Ok`: LaunchServices started a new instance that is not this process; it still has to
    ///   dial the rendezvous.
    /// * `Err(Timeout)`: no answer yet. A successor may still appear; keep rollback idempotent
    ///   against a late dial.
    /// * any other `Err`: nothing usable was launched; fall back with the windows untouched.
    pub(crate) fn wait(&self, budget: Duration) -> Result<LaunchedSuccessor, LaunchError> {
        #[cfg(target_os = "macos")]
        {
            self.watch.wait(budget)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = budget;
            Err(LaunchError::Unsupported)
        }
    }

    /// Has LaunchServices already answered that the launched process exited before it checked in
    /// ([`launch_answer_proves_exit`])? A peek, polled by the dial wait: it never takes the
    /// answer, so the corroboration after a dial ([`Self::wait`]) still reads it.
    #[must_use]
    #[cfg(target_os = "macos")]
    pub(crate) fn exited_before_check_in(&self) -> bool {
        self.watch.slot.peek(launch_answer_proves_exit)
    }

    /// The pid LaunchServices' answer names for the launched process, once it has answered with
    /// one ([`launch_answer_names_pid`]). A peek, like [`Self::exited_before_check_in`]: the dial
    /// wait asks the kernel about that pid (`app_update_handoff::NamedSuccessorWatch`).
    #[must_use]
    #[cfg(target_os = "macos")]
    pub(crate) fn named_pid(&self) -> Option<i32> {
        self.watch.slot.peek(launch_answer_names_pid)
    }
}

/// The pure half: everything that can be refused without asking the system
/// anything (bar one filesystem stat, which is advisory).
fn validate_request(
    bundle: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
) -> Result<LaunchRequest, LaunchError> {
    let shown = bundle.display().to_string();
    let Some(path) = bundle.to_str() else {
        return Err(LaunchError::BundlePath {
            path: shown,
            fault: TextFault::NotUtf8,
        });
    };
    if path.contains('\0') {
        return Err(LaunchError::BundlePath {
            path: shown,
            fault: TextFault::InteriorNul,
        });
    }
    if !bundle.is_absolute() {
        return Err(LaunchError::BundlePathNotAbsolute(shown));
    }
    // `Foo.APP` is the same bundle to LaunchServices, so the check is
    // case-insensitive; the extension, not the whole name, because the bundle is
    // whatever the updater staged, not necessarily `aterm.app`.
    if !bundle
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    {
        return Err(LaunchError::BundlePathNotAnAppBundle(shown));
    }
    if !bundle.is_dir() {
        return Err(LaunchError::BundleMissing(shown));
    }

    let mut checked_arguments = Vec::with_capacity(arguments.len());
    for (index, argument) in arguments.iter().enumerate() {
        let Some(argument) = argument.to_str() else {
            return Err(LaunchError::Argument {
                index,
                fault: TextFault::NotUtf8,
            });
        };
        if argument.contains('\0') {
            return Err(LaunchError::Argument {
                index,
                fault: TextFault::InteriorNul,
            });
        }
        checked_arguments.push(argument.to_string());
    }

    let mut checked_environment: Vec<(String, String)> = Vec::with_capacity(environment.len());
    for (index, (key, value)) in environment.iter().enumerate() {
        let Some(key) = key.to_str() else {
            return Err(LaunchError::EnvKey {
                index,
                fault: TextFault::NotUtf8,
            });
        };
        if let Some(fault) = env_key_fault(key) {
            return Err(LaunchError::EnvKey { index, fault });
        }
        if checked_environment.iter().any(|(seen, _)| seen == key) {
            return Err(LaunchError::EnvKeyDuplicated(key.to_string()));
        }
        let Some(value) = value.to_str() else {
            return Err(LaunchError::EnvValue {
                key: key.to_string(),
                fault: TextFault::NotUtf8,
            });
        };
        if value.contains('\0') {
            return Err(LaunchError::EnvValue {
                key: key.to_string(),
                fault: TextFault::InteriorNul,
            });
        }
        checked_environment.push((key.to_string(), value.to_string()));
    }

    Ok(LaunchRequest {
        bundle: path.to_string(),
        arguments: checked_arguments,
        environment: checked_environment,
    })
}

/// What is wrong with an environment key, if anything. The faults are checked in order (empty,
/// NUL, `=`) so the message names the most fundamental problem.
fn env_key_fault(key: &str) -> Option<TextFault> {
    if key.is_empty() {
        Some(TextFault::Empty)
    } else if key.contains('\0') {
        Some(TextFault::InteriorNul)
    } else if key.contains('=') {
        Some(TextFault::Separator)
    } else {
        None
    }
}

/// Off macOS there is no LaunchServices and so no per-instance launchd job to be had. The
/// request is still validated first ([`begin_launch`]) and named in the log, so a fallback says
/// which bundle it declined.
#[cfg(not(target_os = "macos"))]
fn begin_launch_validated(
    request: &LaunchRequest,
    _activate: bool,
) -> Result<LaunchInFlight, LaunchError> {
    aterm_log::warn!(
        "no LaunchServices on this platform: declining to launch {} as its own job \
         ({} argument(s), {} environment key(s)); the caller keeps its windows and \
         falls back to the fork lane",
        request.bundle,
        request.arguments.len(),
        request.environment.len(),
    );
    Err(LaunchError::Unsupported)
}

/// The AppKit half.
#[cfg(target_os = "macos")]
fn begin_launch_validated(
    request: &LaunchRequest,
    activate: bool,
) -> Result<LaunchInFlight, LaunchError> {
    use std::ffi::c_void;

    use aterm_objc::{Bool, Id, Obj, RcBlock, Sel, autoreleasepool, class, sel};

    use crate::appkit::{self, MainThread};

    // Before anything is launched: a refusal here leaves the machine as it was, whereas one
    // after the call would abandon a running instance and hand the user a second window.
    if MainThread::new().is_some() {
        return Err(LaunchError::WouldBlockMainThread);
    }

    // A Darwin pid is far inside i32; `-1` is the impossible-conversion fallback
    // and is safe because it only disables the substitution check below (which
    // compares against a real pid), while the `pid > 0` check still stands.
    let own_pid = i32::try_from(std::process::id()).unwrap_or(-1);
    let watch = LaunchWatch::pending();
    let slot = Arc::clone(&watch.slot);
    // The completion body is bound first and wrapped second, so its own sends keep their own
    // `unsafe` block: `unsafe { RcBlock::new2(|..| ..) }` would put the whole closure inside the
    // constructor's unsafe context, making every SAFETY comment in it decorative (and earning an
    // `unused_unsafe` warning that says so).
    let completion = move |app: Id, error: Id| {
        // SAFETY: LaunchServices hands the completion a live, autoreleased
        // `NSRunningApplication` or a live `NSError`, valid for the call and borrowed only
        // here. Reading them off the main thread is sound: `NSRunningApplication` is not
        // main-thread-only (module docs) and an `NSError` is immutable. `-domain` and
        // `-localizedDescription` are `-(NSString *)`, `-code` is `-(NSInteger)`,
        // `-processIdentifier` is `-(pid_t)` (an `int`, not `NSInteger`, hence the explicit
        // prototype) and `-isTerminated` is `-(BOOL)`.
        let answer = unsafe {
            if !error.is_null() {
                // An error is authoritative even if an application also
                // arrives: the only safe reading of "launched, but here is
                // why it failed" is that we do not have a successor we may
                // commit to.
                Err(LaunchError::Rejected {
                    domain: appkit::nsstring_to_rust(appkit::send_id(error, sel!(domain))),
                    code: appkit::send_isize(error, sel!(code)),
                    message: appkit::nsstring_to_rust(appkit::send_id(
                        error,
                        sel!(localizedDescription),
                    )),
                })
            } else if app.is_null() {
                Err(LaunchError::NoApplication)
            } else {
                let pid_of: unsafe extern "C-unwind" fn(Id, Sel) -> i32 = aterm_objc::checked_msg();
                let terminated_of: unsafe extern "C-unwind" fn(Id, Sel) -> Bool =
                    aterm_objc::checked_msg();
                admit_launch_answer(
                    pid_of(app, sel!(processIdentifier)),
                    terminated_of(app, sel!(isTerminated)).as_bool(),
                    own_pid,
                )
            }
        };
        slot.deliver(answer);
    };
    // SAFETY: the block's `(id, id) -> void` is exactly the completion signature
    // `-openApplicationAtURL:configuration:completionHandler:` declares, and the closure cannot
    // unwind past the guard `new2` installs.
    let Some(handler) = (unsafe { RcBlock::new2(completion) }) else {
        return Err(LaunchError::ForeignObject("completion handler"));
    };

    // The `NSString`s are built into owned `Obj`s first and only then handed to the
    // array/dictionary constructors as raw `id`s, so each is alive across the call that retains
    // it.
    let Some(path) = appkit::nsstring(&request.bundle) else {
        return Err(LaunchError::ForeignObject("bundle path"));
    };
    let Some(arg_objs) = request
        .arguments
        .iter()
        .map(|argument| appkit::nsstring(argument))
        .collect::<Option<Vec<Obj>>>()
    else {
        return Err(LaunchError::ForeignObject("arguments"));
    };
    let Some(key_objs) = request
        .environment
        .iter()
        .map(|(key, _)| appkit::nsstring(key))
        .collect::<Option<Vec<Obj>>>()
    else {
        return Err(LaunchError::ForeignObject("environment keys"));
    };
    let Some(value_objs) = request
        .environment
        .iter()
        .map(|(_, value)| appkit::nsstring(value))
        .collect::<Option<Vec<Obj>>>()
    else {
        return Err(LaunchError::ForeignObject("environment values"));
    };
    let arg_ids = arg_objs.iter().map(Obj::id).collect::<Vec<Id>>();
    let key_ids = key_objs.iter().map(Obj::id).collect::<Vec<Id>>();
    let value_ids = value_objs.iter().map(Obj::id).collect::<Vec<Id>>();

    // SAFETY: standard AppKit construction, none of it main-thread-only (module docs).
    // `+arrayWithObjects:count:` is `-(id)(const id *, NSUInteger)` and
    // `+dictionaryWithObjects:forKeys:count:` is `-(id)(const id *, const id *, NSUInteger)`;
    // both copy the C arrays and retain the elements, which the `Vec<Obj>`s above keep alive
    // across the call. `+fileURLWithPath:isDirectory:` is `-(id)(NSString *, BOOL)`, handed a
    // live string (a `.app` is a directory, so `true` is the truth and saves a stat).
    // `+configuration` mints a fresh `NSWorkspaceOpenConfiguration` and every setter below is
    // `-(void)(BOOL)` or `-(void)(id)` on it. Everything returned is autoreleased into the pool
    // this body runs in.
    autoreleasepool(|_| unsafe {
        let array_with: unsafe extern "C-unwind" fn(Id, Sel, *const Id, usize) -> Id =
            aterm_objc::checked_msg();
        let arguments = array_with(
            class(c"NSArray").as_id(),
            sel!(arrayWithObjects:count:),
            arg_ids.as_ptr(),
            arg_ids.len(),
        );
        let dict_with: unsafe extern "C-unwind" fn(Id, Sel, *const Id, *const Id, usize) -> Id =
            aterm_objc::checked_msg();
        let environment = dict_with(
            class(c"NSDictionary").as_id(),
            sel!(dictionaryWithObjects:forKeys:count:),
            value_ids.as_ptr(),
            key_ids.as_ptr(),
            key_ids.len(),
        );
        let file_url: unsafe extern "C-unwind" fn(Id, Sel, Id, Bool) -> Id =
            aterm_objc::checked_msg();
        let url = file_url(
            class(c"NSURL").as_id(),
            sel!(fileURLWithPath:isDirectory:),
            path.id(),
            Bool::YES,
        );
        let configuration = appkit::send_id(
            class(c"NSWorkspaceOpenConfiguration").as_id(),
            sel!(configuration),
        );
        if arguments.is_null() || environment.is_null() || url.is_null() || configuration.is_null()
        {
            return;
        }
        // The point of this lane: a second instance of a running bundle, with a launchd
        // application job of its own.
        appkit::send_v_bool(configuration, sel!(setCreatesNewApplicationInstance:), true);
        // Belt to that brace: substitution would hand back the running instance, this process,
        // and no successor would exist.
        appkit::send_v_bool(
            configuration,
            sel!(setAllowsRunningApplicationSubstitution:),
            false,
        );
        // A self-relaunch is not something the user "opened"; keep it out of
        // Recent Items.
        appkit::send_v_bool(configuration, sel!(setAddsToRecentItems:), false);
        // No system prompt may appear on a handoff deadline: a modal
        // quarantine/consent dialog would burn the whole budget and strand the
        // user mid-update with the answer still pending.
        appkit::send_v_bool(configuration, sel!(setPromptsUserIfNeeded:), false);
        // Set explicitly either way: the handoff passes `false` and activates the successor at
        // the Commit's write instead (`activate_running`).
        appkit::send_v_bool(configuration, sel!(setActivates:), activate);
        appkit::send_v_id(configuration, sel!(setArguments:), arguments);
        appkit::send_v_id(configuration, sel!(setEnvironment:), environment);
        let workspace = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
        if workspace.is_null() {
            return;
        }
        // The completion handler is a BLOCK, so the parameter is spelled
        // `*mut c_void` rather than `Id`: a block is `@?` to the runtime, not
        // `@`. It does not matter for a SEND — nothing reads an encoding here —
        // but writing it as an object would be the shape that, in a DECLARED
        // method, registers the wrong letter.
        let open: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, *mut c_void) =
            aterm_objc::checked_msg();
        open(
            workspace,
            sel!(openApplicationAtURL:configuration:completionHandler:),
            url,
            configuration,
            handler.as_ptr(),
        );
    });

    // The handler may be dropped here: AppKit copies a completion handler, and the block holds
    // its own `Arc` to the slot (`LaunchWatch`).
    drop(handler);
    Ok(LaunchInFlight { watch })
}

/// Turn the completion's `processIdentifier` into a successor, or say why it is not one: `-1`
/// (`NSRunningApplication`'s "unknown") is no pid, and this process's own pid means
/// LaunchServices substituted the running instance. Pure, so both refusals are unit-testable.
#[cfg(target_os = "macos")]
fn admit_launched_pid(pid: i32, own_pid: i32) -> Result<LaunchedSuccessor, LaunchError> {
    if pid <= 0 {
        return Err(LaunchError::PidUnknown(pid));
    }
    if pid == own_pid {
        return Err(LaunchError::SubstitutedRunningInstance(pid));
    }
    Ok(LaunchedSuccessor { pid })
}

/// The completion's reading of an `NSRunningApplication`: its `processIdentifier` and whether
/// LaunchServices already reports it `isTerminated`.
///
/// Only an unknown pid on a terminated application changes meaning: it is
/// [`LaunchError::ExitedBeforeCheckIn`], the measured answer for a process that died before
/// checking in, and after a dial it refuses a dialer that is still alive
/// (`app_update_handoff::LaunchCorroboration`). A known pid is admitted whatever `terminated`
/// says: before the dial the wait asks the kernel whether that pid is gone
/// (`app_update_handoff::NamedSuccessorWatch`), which knows exactly.
#[cfg(target_os = "macos")]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_launched_dial_wait",
        action = "AnswerExited",
        project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_launched_dial_wait",
        action = "AnswerNamed",
        project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_launched_dial_wait",
        action = "AnswerOther",
        project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
    )
)]
fn admit_launch_answer(
    pid: i32,
    terminated: bool,
    own_pid: i32,
) -> Result<LaunchedSuccessor, LaunchError> {
    if pid <= 0 && terminated {
        return Err(LaunchError::ExitedBeforeCheckIn);
    }
    admit_launched_pid(pid, own_pid)
}

/// Does LaunchServices' answer prove the launched process is gone, so no dial can come?
///
/// `true` only for [`LaunchError::ExitedBeforeCheckIn`]. An answer naming a pid is a process that
/// checked in, or one LaunchServices stopped waiting for while it lived (whether it died since is
/// the kernel's to say: [`launch_answer_names_pid`]); an error is not death (a successor that
/// already dialled can still be answered with one); no answer is no answer. Read only by the
/// pre-dial wait (`app_update_handoff::await_launched_dial`).
#[must_use]
#[cfg(target_os = "macos")]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_launched_dial_wait",
        action = "EndOnExit",
        project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
    )
)]
pub(crate) fn launch_answer_proves_exit(
    answer: Option<&Result<LaunchedSuccessor, LaunchError>>,
) -> bool {
    matches!(answer, Some(Err(LaunchError::ExitedBeforeCheckIn)))
}

/// The pid LaunchServices' answer names as the launched process: the one process the outgoing
/// process may watch, before the dial, for a death LaunchServices will not report. `Some` only
/// for an admitted [`LaunchedSuccessor`]. Measured with stand-in bundles, such an answer comes at
/// check-in (during `+sharedApplication`) or at LaunchServices' ~30 s check-in timeout, and a
/// process that dies after it is answered gets nothing more. Read only by the pre-dial wait
/// (`app_update_handoff::await_launched_dial`).
#[must_use]
#[cfg(target_os = "macos")]
pub(crate) fn launch_answer_names_pid(
    answer: Option<&Result<LaunchedSuccessor, LaunchError>>,
) -> Option<i32> {
    match answer {
        Some(Ok(successor)) => Some(successor.pid()),
        _ => None,
    }
}

/// The one-slot answer the completion block fills and the caller waits on.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct LaunchSlot {
    answer: Mutex<Option<Result<LaunchedSuccessor, LaunchError>>>,
    answered: Condvar,
}

#[cfg(target_os = "macos")]
impl LaunchSlot {
    /// Record the first answer and wake the waiter. Later answers are dropped: AppKit calls a
    /// completion handler once, and a second call must never overwrite an answer already acted on.
    fn deliver(&self, answer: Result<LaunchedSuccessor, LaunchError>) {
        let mut slot = self.answer.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(answer);
            self.answered.notify_all();
        }
    }

    /// Apply `read` to the answer in place, delivered or not, without taking it: the answer stays
    /// for the one [`LaunchWatch::wait`] that consumes it.
    fn peek<T>(
        &self,
        read: impl FnOnce(Option<&Result<LaunchedSuccessor, LaunchError>>) -> T,
    ) -> T {
        let slot = self.answer.lock().unwrap_or_else(|p| p.into_inner());
        read(slot.as_ref())
    }
}

/// A launch in flight.
///
/// Deliberately outlives the caller's interest in it: the block holds its own
/// `Arc`, so a completion that arrives after a [`LaunchError::Timeout`] writes
/// into a slot nobody reads instead of touching freed memory.
#[cfg(target_os = "macos")]
struct LaunchWatch {
    slot: Arc<LaunchSlot>,
}

// Test-only: how many times [`LaunchWatch::wait`] parked on the condvar on this thread, so a test
// proves a zero budget is a poll by counting parks instead of timing them.
#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static CONDVAR_PARKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(all(test, target_os = "macos"))]
impl LaunchInFlight {
    /// A launch whose LaunchServices answer is scripted: `Some(pid)` is delivered
    /// at once, `None` never answers — so the handoff's corroboration arms can be
    /// driven without LaunchServices.
    pub(crate) fn scripted(answer: Option<i32>) -> Self {
        let watch = LaunchWatch::pending();
        if let Some(pid) = answer {
            watch.slot.deliver(Ok(LaunchedSuccessor { pid }));
        }
        Self { watch }
    }

    /// Deliver `answer` as the completion block would — at once, or from
    /// another thread while the caller waits.
    pub(crate) fn answer_for_test(&self, answer: Result<LaunchedSuccessor, LaunchError>) {
        self.watch.slot.deliver(answer);
    }

    /// The completion's own reading of an `NSRunningApplication` (`processIdentifier`,
    /// `isTerminated`), so a test can script LaunchServices with the answers it gives.
    pub(crate) fn admitted_for_test(
        pid: i32,
        terminated: bool,
        own_pid: i32,
    ) -> Result<LaunchedSuccessor, LaunchError> {
        admit_launch_answer(pid, terminated, own_pid)
    }
}

#[cfg(target_os = "macos")]
impl LaunchWatch {
    fn pending() -> Self {
        Self {
            slot: Arc::new(LaunchSlot::default()),
        }
    }

    /// Block until the launch is answered or `budget` is spent. The budget is measured from entry
    /// and re-derived on every wakeup, so spurious wakeups cannot extend it.
    fn wait(&self, budget: Duration) -> Result<LaunchedSuccessor, LaunchError> {
        let started = Instant::now();
        let mut slot = self.slot.answer.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(answer) = slot.take() {
                return answer;
            }
            let Some(remaining) = remaining_budget(budget, started.elapsed()) else {
                return Err(LaunchError::Timeout(budget));
            };
            #[cfg(test)]
            CONDVAR_PARKS.with(|parks| parks.set(parks.get() + 1));
            let (reacquired, _) = self
                .slot
                .answered
                .wait_timeout(slot, remaining)
                .unwrap_or_else(|p| p.into_inner());
            slot = reacquired;
        }
    }
}

/// How long is left of `budget` after `waited`, or `None` once it is spent. `None` rather than
/// `Some(ZERO)` for an exactly-spent budget, because `Condvar::wait_timeout(ZERO)` returns at
/// once and a caller would spin the deadline; saturating because `waited` can exceed `budget`
/// (the thread may be descheduled), and a panic there would be in a worker holding the
/// user's sessions.
#[cfg(target_os = "macos")]
fn remaining_budget(budget: Duration, waited: Duration) -> Option<Duration> {
    budget.checked_sub(waited).filter(|left| !left.is_zero())
}

#[cfg(test)]
mod tests {
    use super::{LaunchError, TextFault, validate_request};
    use std::ffi::OsString;
    use std::path::PathBuf;

    /// A real directory named `<something>.app`, which is all the pure
    /// validation can ask of a bundle. NOTHING in this file's tests launches an
    /// application.
    ///
    /// The bundle lives as long as the returned guard: keep it bound for the
    /// test's whole body.
    fn scratch_bundle(name: &str) -> (aterm_tempfile::TempDir, PathBuf) {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-launch-successor-{name}-"))
            .tempdir()
            .expect("create the scratch root");
        let bundle = root.path().join("Successor.app");
        std::fs::create_dir_all(&bundle).expect("create the scratch bundle directory");
        (root, bundle)
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(key, value)| (OsString::from(*key), OsString::from(*value)))
            .collect()
    }

    /// A bundle path is refused before anything is launched: relative
    /// (LaunchServices resolves nothing against our working directory), not a
    /// `.app` (checked before the filesystem, so the message names the wrong
    /// kind of target rather than a missing file), or absent.
    #[test]
    fn a_malformed_bundle_path_is_refused() {
        for (path, want) in [
            (
                "Successor.app",
                LaunchError::BundlePathNotAbsolute("Successor.app".to_string()),
            ),
            (
                "/nowhere/successor.tar.gz",
                LaunchError::BundlePathNotAnAppBundle("/nowhere/successor.tar.gz".to_string()),
            ),
            (
                "/nowhere/aterm-does-not-exist.app",
                LaunchError::BundleMissing("/nowhere/aterm-does-not-exist.app".to_string()),
            ),
        ] {
            assert_eq!(
                validate_request(&PathBuf::from(path), &[], &[]),
                Err(want),
                "{path}"
            );
        }
    }

    /// The request carries exactly the caller's keys: the environment merges, so this module
    /// never takes on keeping `PATH`/`HOME`/`XPC_*` alive.
    #[test]
    fn a_well_formed_request_carries_exactly_the_keys_it_was_given() {
        let (_root, bundle) = scratch_bundle("well-formed");
        let request = validate_request(
            &bundle,
            &[OsString::from("--headless"), OsString::from("-e")],
            &env(&[
                ("ATERM_SEAMLESS_NONCE", "0123456789abcdef"),
                ("ATERM_UPDATED_FROM", "1785910394"),
            ]),
        )
        .expect("a real .app directory with sane argv and env validates");

        assert_eq!(request.bundle, bundle.to_str().unwrap());
        assert_eq!(request.arguments, vec!["--headless", "-e"]);
        assert_eq!(
            request.environment,
            vec![
                (
                    "ATERM_SEAMLESS_NONCE".to_string(),
                    "0123456789abcdef".to_string()
                ),
                ("ATERM_UPDATED_FROM".to_string(), "1785910394".to_string()),
            ],
            "only the caller's keys ride the launch; the rest MERGES from this process"
        );
    }

    /// An environment no shell could carry is refused. A NUL in a VALUE is
    /// the quiet one: every C-string consumer past `execve` would truncate
    /// there. A repeated key is refused rather than collapsed: an
    /// `NSDictionary` keeps one of the two, so the successor's view of a
    /// handoff would depend on the caller's vector order.
    #[test]
    fn a_malformed_environment_is_refused() {
        let (_root, bundle) = scratch_bundle("bad-env");
        for (pairs, expected) in [
            (
                env(&[("", "value")]),
                LaunchError::EnvKey {
                    index: 0,
                    fault: TextFault::Empty,
                },
            ),
            (
                env(&[("ATERM_A", "a"), ("ATERM_B=C", "b")]),
                LaunchError::EnvKey {
                    index: 1,
                    fault: TextFault::Separator,
                },
            ),
            (
                env(&[("ATERM\0B", "b")]),
                LaunchError::EnvKey {
                    index: 0,
                    fault: TextFault::InteriorNul,
                },
            ),
            (
                env(&[("ATERM_SEAMLESS_NONCE", "ab\0cd")]),
                LaunchError::EnvValue {
                    key: "ATERM_SEAMLESS_NONCE".to_string(),
                    fault: TextFault::InteriorNul,
                },
            ),
            (
                env(&[
                    ("ATERM_SEAMLESS_NONCE", "first"),
                    ("ATERM_OTHER", "x"),
                    ("ATERM_SEAMLESS_NONCE", "second"),
                ]),
                LaunchError::EnvKeyDuplicated("ATERM_SEAMLESS_NONCE".to_string()),
            ),
        ] {
            assert_eq!(
                validate_request(&bundle, &[], &pairs),
                Err(expected),
                "{pairs:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn text_that_is_not_utf8_is_refused_wherever_it_appears() {
        use std::os::unix::ffi::OsStringExt;

        let (_root, bundle) = scratch_bundle("not-utf8");
        let invalid = || OsString::from_vec(vec![0x61, 0xff, 0x62]);

        assert_eq!(
            validate_request(&bundle, &[OsString::from("--ok"), invalid()], &[]),
            Err(LaunchError::Argument {
                index: 1,
                fault: TextFault::NotUtf8,
            }),
        );
        assert_eq!(
            validate_request(&bundle, &[], &[(invalid(), OsString::from("v"))]),
            Err(LaunchError::EnvKey {
                index: 0,
                fault: TextFault::NotUtf8,
            }),
        );
        assert_eq!(
            validate_request(&bundle, &[], &[(OsString::from("ATERM_K"), invalid())]),
            Err(LaunchError::EnvValue {
                key: "ATERM_K".to_string(),
                fault: TextFault::NotUtf8,
            }),
        );
        assert_eq!(
            validate_request(
                &PathBuf::from(OsString::from_vec(b"/\xff.app".to_vec())),
                &[],
                &[]
            ),
            Err(LaunchError::BundlePath {
                path: "/\u{fffd}.app".to_string(),
                fault: TextFault::NotUtf8,
            }),
        );
    }

    /// Off macOS the request is still validated — same refusals, same tests —
    /// and only then is the platform refused.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_the_launcher_refuses_the_platform_and_not_the_request() {
        use super::launch_app_bundle;
        use std::time::Duration;

        let (_root, bundle) = scratch_bundle("unsupported");
        assert_eq!(
            launch_app_bundle(&bundle, &[], &[], Duration::from_millis(10)),
            Err(LaunchError::Unsupported),
            "a platform with no LaunchServices says so, and launches nothing"
        );
        assert_eq!(
            launch_app_bundle(
                &PathBuf::from("relative.app"),
                &[],
                &[],
                Duration::from_millis(10)
            ),
            Err(LaunchError::BundlePathNotAbsolute(
                "relative.app".to_string()
            )),
            "a malformed request is caught on every platform, not only where it can launch"
        );
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::super::{
            CONDVAR_PARKS, LaunchError, LaunchWatch, LaunchedSuccessor, admit_launch_answer,
            admit_launched_pid, launch_answer_proves_exit, remaining_budget,
        };
        use std::cell::Cell;
        use std::time::{Duration, Instant};

        #[test]
        fn a_spent_budget_never_hands_back_a_zero_length_wait() {
            let budget = Duration::from_millis(100);
            assert_eq!(
                remaining_budget(budget, Duration::from_millis(40)),
                Some(Duration::from_millis(60)),
                "an unspent budget hands back what is left"
            );
            assert_eq!(
                remaining_budget(budget, budget),
                None,
                "an exactly-spent budget is spent, not a zero-length wait to spin on"
            );
            assert_eq!(
                remaining_budget(budget, Duration::from_secs(9)),
                None,
                "an overspent budget saturates instead of panicking in the worker"
            );
            assert_eq!(
                remaining_budget(Duration::ZERO, Duration::ZERO),
                None,
                "a zero budget is already spent"
            );
        }

        /// No AppKit is touched for an impossible pid: activation at Commit is
        /// aimed only at a corroborated candidate.
        #[test]
        fn activation_refuses_an_impossible_pid_without_touching_appkit() {
            assert!(!crate::app_launch_successor::activate_running(0));
            assert!(!crate::app_launch_successor::activate_running(-1));
            assert!(!crate::app_launch_successor::activate_running(1));
        }

        /// A zero budget is a poll: it never parks, answers what is already delivered, and
        /// reports `Timeout` at once otherwise. Parks are counted, not timed; the wall bound is
        /// only a hang detector.
        #[test]
        fn a_zero_budget_wait_is_a_poll_that_never_sleeps() {
            let parks = CONDVAR_PARKS.with(Cell::get);
            let started = Instant::now();
            assert_eq!(
                crate::app_launch_successor::LaunchInFlight::scripted(None).wait(Duration::ZERO),
                Err(LaunchError::Timeout(Duration::ZERO))
            );
            assert_eq!(
                CONDVAR_PARKS.with(Cell::get),
                parks,
                "a zero budget parked on the condvar instead of polling"
            );
            assert!(
                started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "a zero budget waited on (a hang detector): {:?}",
                started.elapsed()
            );
            assert_eq!(
                crate::app_launch_successor::LaunchInFlight::scripted(Some(7))
                    .wait(Duration::ZERO)
                    .map(crate::app_launch_successor::LaunchedSuccessor::pid),
                Ok(7)
            );
            assert_eq!(
                CONDVAR_PARKS.with(Cell::get),
                parks,
                "a delivered answer is read without parking"
            );
            // The count is live, so the zeros above are not vacuous: an
            // unanswered budget parks. The budgets escalate because a thread
            // descheduled past its whole budget before the first check is
            // correctly told `Timeout` without a park.
            let live = [1, 10, 100, 1_000].into_iter().any(|ms| {
                let budget = Duration::from_millis(ms);
                assert_eq!(
                    crate::app_launch_successor::LaunchInFlight::scripted(None).wait(budget),
                    Err(LaunchError::Timeout(budget))
                );
                CONDVAR_PARKS.with(Cell::get) > parks
            });
            assert!(live, "an unanswered budget never parked: the count is dead");
        }

        #[test]
        fn an_answer_from_another_thread_ends_the_wait() {
            let watch = LaunchWatch::pending();
            let slot = std::sync::Arc::clone(&watch.slot);
            let completion = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(10));
                slot.deliver(admit_launched_pid(4321, 4322));
            });
            // A hang detector, never a latency budget: the answer must end the wait, so a slow
            // machine cannot turn this into a timeout.
            let answer = watch.wait(aterm_spec::wait::MUST_HAPPEN);
            completion.join().expect("the completion thread finishes");
            assert_eq!(answer.map(LaunchedSuccessor::pid), Ok(4321));
        }

        #[test]
        fn only_the_first_answer_is_kept() {
            let watch = LaunchWatch::pending();
            watch.slot.deliver(admit_launched_pid(11, 12));
            watch.slot.deliver(Err(LaunchError::NoApplication));
            assert_eq!(
                watch
                    .wait(Duration::from_millis(10))
                    .map(LaunchedSuccessor::pid),
                Ok(11),
                "a second completion must not overwrite the answer already acted on"
            );
        }

        #[test]
        fn an_unusable_pid_is_refused_instead_of_being_waited_on() {
            assert_eq!(
                admit_launched_pid(-1, 900),
                Err(LaunchError::PidUnknown(-1)),
                "NSRunningApplication answers -1 when it does not know a pid"
            );
            assert_eq!(
                admit_launched_pid(0, 900),
                Err(LaunchError::PidUnknown(0)),
                "0 is not a pid this launch could have produced"
            );
            assert_eq!(
                admit_launched_pid(900, 900),
                Err(LaunchError::SubstitutedRunningInstance(900)),
                "our own pid means LaunchServices substituted the running instance"
            );
            assert_eq!(
                admit_launched_pid(901, 900).map(LaunchedSuccessor::pid),
                Ok(901),
            );
        }

        /// Only an application LaunchServices already reports terminated, with no pid, reads as
        /// death.
        #[test]
        fn only_a_terminated_application_with_no_pid_reads_as_death() {
            assert_eq!(
                admit_launch_answer(-1, true, 900),
                Err(LaunchError::ExitedBeforeCheckIn)
            );
            assert_eq!(
                admit_launch_answer(-1, false, 900),
                Err(LaunchError::PidUnknown(-1)),
                "an unknown pid on a live application is not death"
            );
            assert_eq!(
                admit_launch_answer(901, true, 900).map(LaunchedSuccessor::pid),
                Ok(901),
                "a known pid is admitted as before; the corroboration compares it"
            );
            assert_eq!(
                admit_launch_answer(900, false, 900),
                Err(LaunchError::SubstitutedRunningInstance(900))
            );
            assert!(launch_answer_proves_exit(Some(&Err(
                LaunchError::ExitedBeforeCheckIn
            ))));
            for alive in [
                None,
                Some(admit_launch_answer(901, false, 900)),
                Some(admit_launch_answer(901, true, 900)),
                Some(Err(LaunchError::PidUnknown(-1))),
                Some(Err(LaunchError::NoApplication)),
                Some(Err(LaunchError::Timeout(Duration::ZERO))),
                Some(Err(LaunchError::Rejected {
                    domain: "NSCocoaErrorDomain".to_string(),
                    code: 256,
                    message: "a miscellaneous error occurred".to_string(),
                })),
            ] {
                assert!(!launch_answer_proves_exit(alive.as_ref()), "{alive:?}");
            }
        }

        /// The pre-dial poll is a peek: the answer stays for the one `wait` that consumes it,
        /// the corroboration after a dial.
        #[test]
        fn the_exit_peek_leaves_the_answer_for_the_corroboration() {
            let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(None);
            assert!(!in_flight.exited_before_check_in(), "no answer is no death");
            in_flight.answer_for_test(Err(LaunchError::ExitedBeforeCheckIn));
            for _ in 0..3 {
                assert!(in_flight.exited_before_check_in());
            }
            assert_eq!(
                in_flight.wait(Duration::ZERO),
                Err(LaunchError::ExitedBeforeCheckIn),
                "the peeks took nothing"
            );
            let live = crate::app_launch_successor::LaunchInFlight::scripted(Some(4321));
            assert!(!live.exited_before_check_in());
            assert_eq!(
                live.wait(Duration::ZERO).map(LaunchedSuccessor::pid),
                Ok(4321)
            );
        }
    }
}
