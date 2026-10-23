// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Start an application bundle as its own launchd application job.

use std::ffi::OsString;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

/// A successfully launched successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaunchedSuccessor {
    pid: i32,
}

impl LaunchedSuccessor {
    /// The launched instance's pid, as LaunchServices reported it: positive and never this process's own (`admit_launched_pid`, the only non-test constructor, refuses both).
    pub(crate) fn pid(self) -> i32 {
        self.pid
    }
}

/// Why a piece of text cannot be carried to LaunchServices.
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

/// Every way a launch ([`begin_launch`], [`LaunchInFlight::wait`]) can fail to produce a successor; each is the caller's cue to keep the user's windows and fall back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchError {
    /// Not macOS.
    #[cfg(not(target_os = "macos"))]
    Unsupported,
    /// The bundle path itself cannot be expressed as an `NSString`.
    BundlePath {
        path: String,
        fault: TextFault,
    },
    /// LaunchServices resolves nothing relative to a working directory, and the caller's working directory is not the successor's anyway.
    BundlePathNotAbsolute(String),
    /// Only a bundle gets an `application.<bundle-id>.<hex>` job of its own, which is the entire point of this lane; refusing early turns a wrong target into a typed refusal instead of an opaque LaunchServices status a whole deadline later.
    BundlePathNotAnAppBundle(String),
    /// Nothing is at that path (or it is not a directory — every `.app` is one).
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
    /// The same key twice.
    EnvKeyDuplicated(String),
    /// Called on the main thread, where blocking would freeze the terminal.
    #[cfg(target_os = "macos")]
    WouldBlockMainThread,
    /// The budget expired with no answer.
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
    #[cfg(target_os = "macos")]
    NoApplication,
    /// Foundation refused to mint one of the objects the launch needs.
    #[cfg(target_os = "macos")]
    ForeignObject(&'static str),
    /// LaunchServices reported an implausible pid — `NSRunningApplication` answers `-1` when it does not know one.
    #[cfg(target_os = "macos")]
    PidUnknown(i32),
    /// LaunchServices answered with an application that had ALREADY TERMINATED and whose pid it no longer knows (`-1`): the launched process ran and exited before it ever checked in (2026-09-29, measured on macOS 26.6.2 against an ad-hoc-signed stand-in bundle launched exactly as [`begin_launch`] launches).
    #[cfg(target_os = "macos")]
    ExitedBeforeCheckIn,
    /// The answer names this process: LaunchServices substituted the running instance, so no successor exists.
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
/// NUL, no duplicate environment key.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchRequest {
    bundle: String,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
}

/// [`begin_launch`] then [`LaunchInFlight::wait`] for at most `budget`: the off-macOS test's pin of the validate-then-refuse contract.
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) fn launch_app_bundle(
    bundle: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    budget: Duration,
) -> Result<LaunchedSuccessor, LaunchError> {
    let request = validate_request(bundle, arguments, environment)?;
    begin_launch_validated(&request, false)?.wait(budget)
}

/// Validate the request and hand it to LaunchServices, returning the in-flight handle at once so the caller can accept the successor's rendezvous dial while the answer is pending.
pub(crate) fn begin_launch(
    bundle: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    activate: bool,
) -> Result<LaunchInFlight, LaunchError> {
    let request = validate_request(bundle, arguments, environment)?;
    begin_launch_validated(&request, activate)
}

/// What LaunchServices' list of this app says about this process ([`own_launch_record`]).
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnLaunchRecord {
    /// An entry carries this process's pid: Quit & Reopen, the menu bar and TCC find it.
    Named,
    /// No entry carries it and one carries none (`-1`): the stale record an `execve` after a LaunchServices launch leaves ([`LaunchError::PidUnknown`]), which lasts the process's life, and which a launch-record renewal replaces with a fresh process (`launch_record_renewal`).
    Stale,
    /// Neither: no bundle identifier, or no entry without a pid to be this one.
    Unknown,
}

/// How long into a windowed launch [`own_launch_record`] is asked: the stale record formed within about a second of the `execve` in every measured run (2026-10-19), and the launch's own exec comes before the window.
#[cfg(target_os = "macos")]
pub(crate) const OWN_LAUNCH_RECORD_SETTLE: std::time::Duration = std::time::Duration::from_secs(10);

/// Ask LaunchServices for every running instance of this bundle (`+[NSRunningApplication runningApplicationsWithBundleIdentifier:]`) and whether one carries this process's pid.
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
        // SAFETY: `+runningApplicationsWithBundleIdentifier:` is `+(NSArray *) (NSString *)`, +0, valid for this pool; `-count` is `-(NSUInteger)`, `-objectAtIndex:` `-(id)(NSUInteger)` below the count, and `-processIdentifier` `-(pid_t)` on each element.
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

/// Bring the successor to the front at the Commit's write (`App::activate_successor_at_write`, only when an aterm window has focus).
#[cfg(target_os = "macos")]
pub(crate) fn activate_running(pid: i32) -> bool {
    use aterm_objc::{Bool, Id, Sel, autoreleasepool, class, sel};
    if pid <= 1 {
        return false;
    }
    if crate::platform::Windowed::get().is_none() {
        return false;
    }
    /// `NSApplicationActivateIgnoringOtherApps` (AppKit, `NSApplicationActivationOptions`).
    const ACTIVATE_IGNORING_OTHER_APPS: usize = 1 << 1;
    autoreleasepool(|_| {
        // SAFETY: class methods on immortal AppKit classes and instance methods on live objects: `runningApplicationWithProcessIdentifier:` answers nil or an autoreleased instance valid for this pool; `sharedApplication` is this process's own NSApplication (Commit runs on the main thread, where it exists); `respondsToSelector:` is a side-effect-free query; `yieldActivationToApplication:` and `activateWithOptions:` take the instance and a bitmask by value.
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
pub(crate) struct LaunchInFlight {
    #[cfg(target_os = "macos")]
    watch: LaunchWatch,
}

impl LaunchInFlight {
    /// Block until the launch is answered or `budget` is spent.
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

    /// Has LaunchServices already answered that the launched process exited before it checked in ([`launch_answer_proves_exit`])? A peek, polled by the dial wait: it never takes the answer, so the corroboration after a dial ([`Self::wait`]) still reads it.
    #[must_use]
    #[cfg(target_os = "macos")]
    pub(crate) fn exited_before_check_in(&self) -> bool {
        self.watch.slot.peek(launch_answer_proves_exit)
    }

    /// The pid LaunchServices' answer names for the launched process, once it has answered with one ([`launch_answer_names_pid`]).
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

/// What is wrong with an environment key, if anything.
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

/// Off macOS there is no LaunchServices and so no per-instance launchd job to be had.
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

    if MainThread::new().is_some() {
        return Err(LaunchError::WouldBlockMainThread);
    }

    let own_pid = i32::try_from(std::process::id()).unwrap_or(-1);
    let watch = LaunchWatch::pending();
    let slot = Arc::clone(&watch.slot);
    let completion = move |app: Id, error: Id| {
        // SAFETY: LaunchServices hands the completion a live, autoreleased `NSRunningApplication` or a live `NSError`, valid for the call and borrowed only here.
        let answer = unsafe {
            if !error.is_null() {
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
    // SAFETY: the block's `(id, id) -> void` is exactly the completion signature `-openApplicationAtURL:configuration:completionHandler:` declares, and the closure cannot unwind past the guard `new2` installs.
    let Some(handler) = (unsafe { RcBlock::new2(completion) }) else {
        return Err(LaunchError::ForeignObject("completion handler"));
    };

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
        appkit::send_v_bool(configuration, sel!(setCreatesNewApplicationInstance:), true);
        appkit::send_v_bool(
            configuration,
            sel!(setAllowsRunningApplicationSubstitution:),
            false,
        );
        appkit::send_v_bool(configuration, sel!(setAddsToRecentItems:), false);
        appkit::send_v_bool(configuration, sel!(setPromptsUserIfNeeded:), false);
        appkit::send_v_bool(configuration, sel!(setActivates:), activate);
        appkit::send_v_id(configuration, sel!(setArguments:), arguments);
        appkit::send_v_id(configuration, sel!(setEnvironment:), environment);
        let workspace = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
        if workspace.is_null() {
            return;
        }
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

    drop(handler);
    Ok(LaunchInFlight { watch })
}

/// Turn the completion's `processIdentifier` into a successor, or say why it is not one: `-1`
/// (`NSRunningApplication`'s "unknown") is no pid, and this process's own pid means
/// LaunchServices substituted the running instance.
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

/// The pid LaunchServices' answer names as the launched process: the one process the outgoing process may watch, before the dial, for a death LaunchServices will not report.
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
    /// Record the first answer and wake the waiter.
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
#[cfg(target_os = "macos")]
struct LaunchWatch {
    slot: Arc<LaunchSlot>,
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static CONDVAR_PARKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(all(test, target_os = "macos"))]
impl LaunchInFlight {
    /// A launch whose LaunchServices answer is scripted: `Some(pid)` is delivered at once, `None` never answers — so the handoff's corroboration arms can be driven without LaunchServices.
    pub(crate) fn scripted(answer: Option<i32>) -> Self {
        let watch = LaunchWatch::pending();
        if let Some(pid) = answer {
            watch.slot.deliver(Ok(LaunchedSuccessor { pid }));
        }
        Self { watch }
    }

    /// Deliver `answer` as the completion block would — at once, or from another thread while the caller waits.
    pub(crate) fn answer_for_test(&self, answer: Result<LaunchedSuccessor, LaunchError>) {
        self.watch.slot.deliver(answer);
    }

    /// The completion's own reading of an `NSRunningApplication` (`processIdentifier`, `isTerminated`), so a test can script LaunchServices with the answers it gives.
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

    /// Block until the launch is answered or `budget` is spent.
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

/// How long is left of `budget` after `waited`, or `None` once it is spent.
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
    /// validation can ask of a bundle.
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

    /// An environment no shell could carry is refused.
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
        /// reports `Timeout` at once otherwise.
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
