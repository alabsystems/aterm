// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Start an application bundle as its OWN launchd application job.
//!
//! # Why this exists at all
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
//! `tests/handoff_launchd_job.rs` states that property and is the guard.
//!
//! A LaunchServices launch is the fix: it hands the new instance to launchd,
//! which mints it a job — and therefore a bootstrap domain — of its own. Probed
//! on this machine against a real bundle, such an instance is born with
//! `ppid == 1`; a fork child instead keeps the parent's ppid AND the parent's
//! launchd domain, which is exactly the defect.
//!
//! # What this module is, and what it is deliberately not
//!
//! It launches, and it reports. That is the whole contract.
//!
//! * It transfers NO descriptors. A LaunchServices launch inherits none — that
//!   is the premise of the handoff's B4 transport work, not an oversight here —
//!   so the PTY masters and the readiness/Commit pipes must reach the successor
//!   by some other route.
//! * The completion answer is NOT liveness. It says LaunchServices accepted (or
//!   refused) the launch, not that a successor is running, has adopted anything,
//!   or is ready to be committed to. The rendezvous dial is the liveness signal;
//!   see the naming rules in `app_update_handoff::run_handoff_worker`.
//! * It never exits, never signals, never kills, and never touches the outgoing
//!   process's sessions. EVERY failure is a typed [`LaunchError`] handed back to
//!   the caller, whose job is to fall back to the fork lane or roll back through
//!   the cleanup it already owns. A refusal that happens BEFORE the AppKit call
//!   leaves the machine untouched; see "a timeout is not a refusal" below for
//!   the one case that does not.
//!
//! # The environment MERGES — so pass only what is new
//!
//! Measured here, by reading a launched instance's own environment with
//! `ps eww`: the keys set on `NSWorkspaceOpenConfiguration.environment` arrive,
//! AND a canary present only in the LAUNCHING process's environment arrives,
//! alongside `PATH`, `HOME` and the session's `XPC_*` vars (46 in total). So
//! there is no allowlist to maintain and no environment to reconstruct: pass the
//! handoff keys and nothing else. Reconstructing would be actively wrong — it
//! would be this module's job to keep `XPC_SERVICE_NAME` alive, and it would get
//! that wrong eventually.
//!
//! WHAT MUST NOT RIDE THIS ENVIRONMENT: any key whose value is a DESCRIPTOR
//! NUMBER (the handoff's `ATERM_HANDOFF_READY_FD` / `ATERM_HANDOFF_COMMIT_FD`,
//! and the fd column of `ATERM_SEAMLESS_FDS`). Those numbers name entries in the
//! FORK CHILD's table, which a launched process does not have; in a
//! LaunchServices-launched successor the same integers name whatever
//! LaunchServices left there. This module cannot tell a descriptor number from
//! any other digits, so the rule is the caller's to keep — and the reason the
//! transport moves to `SCM_RIGHTS` over the rendezvous socket.
//!
//! # A timeout is not a refusal
//!
//! `openApplicationAtURL:configuration:completionHandler:` is asynchronous, and
//! the caller is a worker thread that needs a definite answer, so
//! [`launch_app_bundle`] blocks — under a deadline it can never exceed, because
//! the handoff's own decision deadline is what keeps the user's windows from
//! being held hostage by a stuck launch.
//!
//! `budget` bounds how long the CALLER waits, never how long LaunchServices
//! takes. `LaunchError::Timeout` therefore means "no answer yet", NOT "no
//! successor": the launch may already have happened, and a process may dial the
//! rendezvous a second later. So the caller must (i) treat the rendezvous dial,
//! not this answer, as proof that a successor exists, and (ii) keep its rollback
//! idempotent against a successor that arrives late — the socket sweep on the
//! rollback paths (`seamless::discard_outgoing`) is what makes that late dial
//! fail closed instead of adopting a session the parent has taken back.
//!
//! An early `Err` from the completion handler is still worth waiting for: it is
//! the earliest possible NEGATIVE answer, and it lets the caller roll back
//! immediately instead of burning the whole dial budget on a launch that
//! LaunchServices already refused.
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
//! This uses the TYPED `NSWorkspace` bindings, so `objc2-app-kit` needs its
//! `NSWorkspace` feature enabled (the crate's list already carries the other two
//! that `openApplicationAtURL:configuration:completionHandler:` is gated on,
//! `NSRunningApplication` and `block2`, and the feature pulls the
//! `NSURL`/`NSError`/`NSArray`/`NSDictionary` bindings it needs from
//! `objc2-foundation` itself). `menu.rs` and `platform.rs` reach NSWorkspace
//! through `class!` + `msg_send!` precisely because that feature was off; an
//! untyped `msg_send!` is a poor trade here, where the call takes a URL, a
//! configuration object, a dictionary and a block whose signature must match
//! what AppKit invokes.

use std::ffi::OsString;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

/// A successfully launched successor.
///
/// The pid is DIAGNOSTIC, not authority. The successor is launchd's child, not
/// ours, so this pid cannot be `wait`ed on, and a pid alone is reusable —
/// nothing may be reaped, signalled or admitted on the strength of it. The
/// kernel-attested identity the handoff acts on is the one `LOCAL_PEERPID`
/// yields at `accept` on the rendezvous socket, cross-checked against the birth
/// record `seamless::AttestedParent` already reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaunchedSuccessor {
    pid: i32,
}

impl LaunchedSuccessor {
    /// The launched instance's pid, as LaunchServices reported it. Always
    /// positive and never this process's own pid — `admit_launched_pid` is the
    /// only constructor of this type and refuses both.
    pub(crate) fn pid(self) -> i32 {
        self.pid
    }
}

/// Why a piece of text cannot be carried to LaunchServices.
///
/// `arguments` and `environment` ride `NSString`s, so a value that is not UTF-8
/// has no representation at all, and one containing a NUL would be silently
/// truncated by every C-string consumer downstream of `execve` — a corrupted
/// handoff key is worse than a refused launch, because the refusal falls back to
/// the fork lane and the corruption does not.
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

/// Every way [`launch_app_bundle`] can decline to produce a successor.
///
/// All of them are the caller's cue to keep the user's windows and fall back —
/// none is ever a reason to proceed. They are split finely on purpose: "the
/// bundle is not on disk" and "LaunchServices refused it" want different
/// remedies, and a single opaque string would let a wiring mistake (a duplicated
/// env key, a relative path) hide inside an operating-system failure.
///
/// EACH REFUSAL EXISTS EXACTLY WHERE IT CAN HAPPEN. A macOS build cannot report
/// `Unsupported` and a build with no LaunchServices cannot report a timeout from
/// one, so those variants are `cfg`'d to their platform: reading the wrong one is
/// then a compile error rather than an arm that silently never runs, and neither
/// build carries a variant nothing in it can construct. The request refusals
/// above the line are platform-free — the request is validated identically
/// everywhere.
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
    /// The same key twice. An `NSDictionary` would silently keep ONE of them,
    /// so which value the successor saw would depend on argument order — a
    /// handoff key resolved by accident is not a handoff.
    EnvKeyDuplicated(String),
    /// Called on the main thread, where blocking would freeze the terminal.
    #[cfg(target_os = "macos")]
    WouldBlockMainThread,
    /// The budget expired with no answer. NOT proof that nothing was launched —
    /// see the module docs.
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
    /// Foundation refused to mint one of the objects the launch needs.
    ///
    /// Only reachable on an allocation failure: every string reaching
    /// `launch_validated` has already passed the text validation above, so an
    /// interior NUL or a bad encoding cannot get this far. It exists because
    /// the first-party layer returns `Option` where the binding crate returned
    /// an infallible constructor, and the honest answer to "Foundation said no"
    /// is a named refusal rather than an `expect`.
    #[cfg(target_os = "macos")]
    ForeignObject(&'static str),
    /// LaunchServices reported an implausible pid — `NSRunningApplication`
    /// answers `-1` when it does not know one.
    #[cfg(target_os = "macos")]
    PidUnknown(i32),
    /// The answer names THIS process: LaunchServices substituted the
    /// already-running instance instead of starting a new one, so no successor
    /// exists and no dial can ever arrive. Caught here so the caller refuses in
    /// milliseconds instead of stalling for its whole dial budget.
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
            Self::SubstitutedRunningInstance(pid) => write!(
                f,
                "LaunchServices answered with the already-running instance (pid {pid}) \
                 instead of starting a new one"
            ),
        }
    }
}

/// A launch request every field of which is already expressible to
/// LaunchServices: valid UTF-8, no interior NUL, no duplicate environment key.
///
/// Validation is separated from the AppKit call so that the refusals — which are
/// the interesting half, because each one is a fallback to the fork lane — are
/// pure, unit-testable, and identical on every platform.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchRequest {
    bundle: String,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
}

/// Launch `bundle` as its own launchd application job and block, for at most
/// `budget`, for LaunchServices' answer.
///
/// `arguments` become the successor's `argv[1..]` (the fork lane's
/// `Command::args(std::env::args_os().skip(1))` has no other equivalent here),
/// and `environment` is MERGED over the environment the launching process
/// already publishes — pass the handoff keys only, never a reconstructed
/// environment, and never a descriptor number (module docs).
///
/// # What each answer means
///
/// * `Ok` — LaunchServices started a NEW instance and it is not this process.
///   The successor still has to dial the rendezvous; this is not that.
/// * `Err(Timeout)` — no answer yet. A successor may still appear; keep the
///   rollback idempotent against a late dial.
/// * every other `Err` — nothing was launched, or nothing usable was; fall back
///   to the fork lane with the user's windows untouched.
///
/// Nothing here mutates process state, so a refusal needs no cleanup of its own.
pub(crate) fn launch_app_bundle(
    bundle: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    budget: Duration,
) -> Result<LaunchedSuccessor, LaunchError> {
    // Validated on EVERY platform, ahead of the platform check, so that a wiring
    // mistake fails the same way (and is caught by the same tests) wherever it is
    // built — the non-macOS stub then refuses the platform, not the request.
    let request = validate_request(bundle, arguments, environment)?;
    launch_validated(&request, budget)
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

/// What is wrong with an environment KEY, if anything.
///
/// Its own function because the three faults are ordered — empty, then NUL, then
/// `=` — and the order is what makes the message name the real problem: an empty
/// key trivially contains neither of the other two, and a key containing both a
/// NUL and an `=` is unusable for the earlier, more fundamental reason.
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

/// Honest refusal off macOS: there is no LaunchServices, so there is no
/// per-instance launchd job to be had, and pretending otherwise would hand the
/// caller a successor it does not have.
///
/// The request is still validated first (see [`launch_app_bundle`]) and named in
/// full here, so a fallback on this platform says which bundle it declined —
/// the same thing a reader of the fork-lane log wants to know — instead of
/// vanishing into a bare error value.
#[cfg(not(target_os = "macos"))]
fn launch_validated(
    request: &LaunchRequest,
    _budget: Duration,
) -> Result<LaunchedSuccessor, LaunchError> {
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
fn launch_validated(
    request: &LaunchRequest,
    budget: Duration,
) -> Result<LaunchedSuccessor, LaunchError> {
    use std::ffi::c_void;

    use aterm_objc::{Bool, Id, Obj, RcBlock, Sel, autoreleasepool, class, sel};

    use crate::appkit::{self, MainThread};

    // BEFORE anything is launched: a refusal here leaves the machine exactly as
    // it was, whereas refusing after the call would abandon a running instance
    // and hand the user a second window.
    if MainThread::new().is_some() {
        return Err(LaunchError::WouldBlockMainThread);
    }

    // A Darwin pid is far inside i32; `-1` is the impossible-conversion fallback
    // and is safe because it only disables the substitution check below (which
    // compares against a real pid), while the `pid > 0` check still stands.
    let own_pid = i32::try_from(std::process::id()).unwrap_or(-1);
    let watch = LaunchWatch::pending();
    let slot = Arc::clone(&watch.slot);
    // The completion body is bound FIRST and wrapped SECOND, so its own sends
    // keep their own `unsafe` block: writing `unsafe { RcBlock::new2(|..| ..) }`
    // puts the whole closure inside the constructor's unsafe context, which
    // makes every SAFETY comment inside it decorative and earns an
    // `unnecessary_unsafe` warning that says so.
    let completion = move |app: Id, error: Id| {
        // SAFETY: LaunchServices hands the completion a live, autoreleased
        // `NSRunningApplication` OR a live `NSError`, valid for the duration
        // of the call; both are borrowed only here and neither is retained
        // past the block. Reading them off the main thread is sound because
        // both classes are thread-safe — see the module docs.
        // `-domain` and `-localizedDescription` are `-(NSString *)`,
        // `-code` is `-(NSInteger)` and `-processIdentifier` is `-(pid_t)`,
        // i.e. `int` and NOT `NSInteger`, which is why it is written out.
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
                let pid_of: unsafe extern "C" fn(Id, Sel) -> i32 = aterm_objc::msg();
                admit_launched_pid(pid_of(app, sel!(processIdentifier)), own_pid)
            }
        };
        slot.deliver(answer);
    };
    // `RcBlock::new2` — the two-argument constructor `aterm-objc`'s block
    // module names this very site for. The prototype is `(id, id) -> void`,
    // which is what LaunchServices calls it with, and `Encode` on both
    // arguments is what makes that statable.
    //
    // SAFETY: the argument and return types are exactly the completion
    // signature `-openApplicationAtURL:configuration:completionHandler:`
    // declares, and the closure cannot unwind past the guard `new2` installs.
    let Some(handler) = (unsafe { RcBlock::new2(completion) }) else {
        return Err(LaunchError::ForeignObject("completion handler"));
    };

    // The `NSString`s are built into owned `Obj`s FIRST and only then handed to
    // the array/dictionary constructors as raw `id`s, so each one is alive
    // across the construction call that retains it. `objc2` got that from its
    // `Retained<NSString>` vectors; here it is the `Vec<Obj>` binding's scope.
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

    // SAFETY: standard AppKit construction, none of it main-thread-only (module
    // docs). `+arrayWithObjects:count:` is `-(id)(const id *, NSUInteger)` and
    // `+dictionaryWithObjects:forKeys:count:` is
    // `-(id)(const id *, const id *, NSUInteger)`; both COPY the C arrays and
    // RETAIN the elements, which the `Vec<Obj>`s above keep alive across the
    // call. `+fileURLWithPath:isDirectory:` is `-(id)(NSString *, BOOL)` and is
    // handed a live string (a `.app` IS a directory, so `true` is the truth and
    // saves LaunchServices a stat). `+configuration` mints a fresh
    // `NSWorkspaceOpenConfiguration` and every setter below is
    // `-(void)(BOOL)` or `-(void)(id)` on it. Everything returned here is
    // AUTORELEASED into the pool this whole body runs in.
    autoreleasepool(|_| unsafe {
        let array_with: unsafe extern "C" fn(Id, Sel, *const Id, usize) -> Id = aterm_objc::msg();
        let arguments = array_with(
            class(c"NSArray").as_id(),
            sel!(arrayWithObjects:count:),
            arg_ids.as_ptr(),
            arg_ids.len(),
        );
        let dict_with: unsafe extern "C" fn(Id, Sel, *const Id, *const Id, usize) -> Id =
            aterm_objc::msg();
        let environment = dict_with(
            class(c"NSDictionary").as_id(),
            sel!(dictionaryWithObjects:forKeys:count:),
            value_ids.as_ptr(),
            key_ids.as_ptr(),
            key_ids.len(),
        );
        let file_url: unsafe extern "C" fn(Id, Sel, Id, Bool) -> Id = aterm_objc::msg();
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
        // THE point of this lane: a second instance of a bundle that is already
        // running, with a launchd application job of its own.
        appkit::send_v_bool(configuration, sel!(setCreatesNewApplicationInstance:), true);
        // Belt to that brace: substitution would hand back the ALREADY-RUNNING
        // instance — this very process — and no successor would exist at all.
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
        // The successor takes over the user's windows, so it — not the outgoing
        // process, which is about to exit — must be the frontmost app. Set
        // explicitly because it is load-bearing, not incidental.
        appkit::send_v_bool(configuration, sel!(setActivates:), true);
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
        let open: unsafe extern "C" fn(Id, Sel, Id, Id, *mut c_void) = aterm_objc::msg();
        open(
            workspace,
            sel!(openApplicationAtURL:configuration:completionHandler:),
            url,
            configuration,
            handler.as_ptr(),
        );
    });

    // TAIL EXPRESSION ON PURPOSE: locals are dropped only after it is evaluated,
    // so `handler` outlives the wait. AppKit copies a completion handler (that
    // is why the binding takes `&Block`), and the block itself only touches an
    // `Arc` that outlives us either way — but a block that is still ours while
    // it can still be called costs nothing and is one less thing to be wrong
    // about.
    watch.wait(budget)
}

/// Turn the completion's `processIdentifier` into a successor, or say why it is
/// not one.
///
/// Split out and pure because both refusals are silent-stall bugs otherwise: a
/// `-1` (which `NSRunningApplication` uses for "unknown") would be committed to
/// as a pid, and a substituted running instance would leave the caller waiting
/// out its whole dial budget for a process that was never started.
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

/// The one-slot answer the completion block fills and the caller waits on.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct LaunchSlot {
    answer: Mutex<Option<Result<LaunchedSuccessor, LaunchError>>>,
    answered: Condvar,
}

#[cfg(target_os = "macos")]
impl LaunchSlot {
    /// Record the FIRST answer and wake the waiter. Later answers are dropped:
    /// AppKit calls a completion handler once, and if it ever called twice the
    /// second call must not be able to overwrite the answer the caller has
    /// already acted on.
    fn deliver(&self, answer: Result<LaunchedSuccessor, LaunchError>) {
        let mut slot = self.answer.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(answer);
            self.answered.notify_all();
        }
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

#[cfg(target_os = "macos")]
impl LaunchWatch {
    fn pending() -> Self {
        Self {
            slot: Arc::new(LaunchSlot::default()),
        }
    }

    /// Block until the launch is answered or `budget` is spent.
    ///
    /// The budget is measured from entry and re-derived on every wakeup, so
    /// spurious wakeups cannot extend it and a slow answer cannot restart it —
    /// the total wait is bounded by `budget` no matter how many times the
    /// condvar fires.
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
            let (reacquired, _) = self
                .slot
                .answered
                .wait_timeout(slot, remaining)
                .unwrap_or_else(|p| p.into_inner());
            slot = reacquired;
        }
    }
}

/// How long is left of `budget` after `waited`, or `None` once it is spent.
///
/// `None` — rather than `Some(ZERO)` — for an exactly-spent budget on purpose:
/// `Condvar::wait_timeout` with a zero timeout is a legal call that returns
/// immediately, so a caller that did not distinguish the two would spin the
/// deadline instead of reporting it. Saturating rather than wrapping matters
/// just as much: `waited` can exceed `budget` (the thread may not be scheduled
/// for the whole interval), and an unchecked subtraction there would panic in a
/// worker holding the user's sessions.
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
    /// application: the AppKit call is exercised by the ignored end-to-end guard
    /// in `tests/handoff_launchd_job.rs`, which owns a real bundle.
    fn scratch_bundle(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aterm-launch-successor-{}-{name}",
            std::process::id()
        ));
        let bundle = root.join("Successor.app");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&bundle).expect("create the scratch bundle directory");
        bundle
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(key, value)| (OsString::from(*key), OsString::from(*value)))
            .collect()
    }

    #[test]
    fn a_relative_bundle_path_is_refused() {
        let refusal = validate_request(&PathBuf::from("Successor.app"), &[], &[]);
        assert_eq!(
            refusal,
            Err(LaunchError::BundlePathNotAbsolute(
                "Successor.app".to_string()
            )),
            "LaunchServices resolves nothing against our working directory"
        );
    }

    #[test]
    fn a_target_that_is_not_a_dot_app_is_refused_before_the_filesystem_is_consulted() {
        // Deliberately a path that does not exist either: the extension check
        // must fire first, so the message names the real problem (the wrong kind
        // of target) instead of blaming a missing file.
        let refusal = validate_request(&PathBuf::from("/nowhere/successor.tar.gz"), &[], &[]);
        assert_eq!(
            refusal,
            Err(LaunchError::BundlePathNotAnAppBundle(
                "/nowhere/successor.tar.gz".to_string()
            )),
        );
    }

    #[test]
    fn a_bundle_that_is_not_on_disk_is_refused() {
        let missing = PathBuf::from("/nowhere/aterm-does-not-exist.app");
        assert_eq!(
            validate_request(&missing, &[], &[]),
            Err(LaunchError::BundleMissing(
                "/nowhere/aterm-does-not-exist.app".to_string()
            )),
        );
    }

    /// The measured merge fact, pinned as a property of the request we build:
    /// what we hand LaunchServices is EXACTLY the keys the caller passed. A
    /// request that grew `PATH`/`HOME`/`XPC_*` of its own would mean this module
    /// had taken on keeping the launching process's environment alive — which it
    /// does not need to do, because the environment merges.
    #[test]
    fn a_well_formed_request_carries_exactly_the_keys_it_was_given() {
        let bundle = scratch_bundle("well-formed");
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

    #[test]
    fn an_environment_key_that_no_shell_could_carry_is_refused() {
        let bundle = scratch_bundle("bad-keys");
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
        ] {
            assert_eq!(validate_request(&bundle, &[], &pairs), Err(expected));
        }
    }

    /// A NUL in a VALUE is the quiet one: `NSString` would carry it, and every
    /// C-string consumer past `execve` would truncate there — the successor
    /// would read a handoff key that authenticates against nothing rather than
    /// falling back.
    #[test]
    fn a_nul_inside_an_environment_value_is_refused() {
        let bundle = scratch_bundle("nul-value");
        assert_eq!(
            validate_request(&bundle, &[], &env(&[("ATERM_SEAMLESS_NONCE", "ab\0cd")])),
            Err(LaunchError::EnvValue {
                key: "ATERM_SEAMLESS_NONCE".to_string(),
                fault: TextFault::InteriorNul,
            }),
        );
    }

    /// REGRESSION SHAPE: an `NSDictionary` silently keeps one of two entries
    /// with the same key, so a doubled key would make the successor's view of a
    /// handoff depend on the order the caller happened to build its vector in.
    #[test]
    fn a_repeated_environment_key_is_refused_rather_than_silently_collapsed() {
        let bundle = scratch_bundle("duplicate-key");
        assert_eq!(
            validate_request(
                &bundle,
                &[],
                &env(&[
                    ("ATERM_SEAMLESS_NONCE", "first"),
                    ("ATERM_OTHER", "x"),
                    ("ATERM_SEAMLESS_NONCE", "second"),
                ])
            ),
            Err(LaunchError::EnvKeyDuplicated(
                "ATERM_SEAMLESS_NONCE".to_string()
            )),
        );
    }

    #[cfg(unix)]
    #[test]
    fn text_that_is_not_utf8_is_refused_wherever_it_appears() {
        use std::os::unix::ffi::OsStringExt;

        let bundle = scratch_bundle("not-utf8");
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

        let bundle = scratch_bundle("unsupported");
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
            LaunchError, LaunchWatch, LaunchedSuccessor, admit_launched_pid, remaining_budget,
        };
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

        #[test]
        fn a_launch_that_is_never_answered_times_out_at_its_budget() {
            let watch = LaunchWatch::pending();
            let budget = Duration::from_millis(60);
            let started = Instant::now();
            let answer = watch.wait(budget);
            assert_eq!(answer, Err(LaunchError::Timeout(budget)));
            assert!(
                started.elapsed() >= budget,
                "the wait must honour the whole budget before declaring a timeout"
            );
        }

        #[test]
        fn an_answer_from_another_thread_ends_the_wait() {
            let watch = LaunchWatch::pending();
            let slot = std::sync::Arc::clone(&watch.slot);
            let completion = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(10));
                slot.deliver(admit_launched_pid(4321, 4322));
            });
            // A generous budget: the assertion is that the ANSWER ends the wait,
            // so a slow machine must not be able to turn this into a timeout.
            let answer = watch.wait(Duration::from_secs(10));
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

        /// Both of these would otherwise be silent stalls: the caller would wait
        /// out its whole dial budget for a successor that does not exist.
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
    }
}
