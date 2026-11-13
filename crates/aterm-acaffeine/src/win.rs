// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm acaffeine` on Windows: keep this PC from its idle sleep while the saved mode is on.
//!
//! There is no mug here. `on` saves the mode ([`crate::state`], the file macOS's mug reads) and
//! makes sure a holder runs: this executable as `acaffeine serve`, started with no console, none
//! of this process's handles and the system directory as its own, so it outlives the terminal
//! that started it. A terminal that runs its programs in a job (an aterm tab's ends every one
//! when the tab closes) has the holder started outside it ([`aterm_pty::start_outside_job`]:
//! under the aterm window, which no tab's close ends); one that cannot leave the job is said
//! to end with the terminal. The holder holds one power request (`PowerCreateRequest`, system
//! required: [`aterm_sysprobe::power::hold`]), which Windows drops with the process, and exits
//! once the mode is off or its span is over. It keeps no timer but the span's end: it waits on
//! a named event that `on` and `off` set after they save, and that a resume from sleep sets,
//! since a span is counted on the clock that runs through a sleep.
//!
//! What runs is told by named events, which Windows removes with the process that made them, so
//! nothing stale survives a holder that was killed: one exists while a holder runs, one while it
//! holds the request. Their names carry a hash of the state directory, so an aterm with another
//! state root (a test's, a lab instance's) has a holder of its own. Each is made for this user
//! whatever the holder's elevation ([`event_sddl`]), so a verb of an unelevated terminal can tell
//! a holder an elevated one started; a holder a verb could not tell fails the verb.
//!
//! The request keeps the system from sleeping on its idle timer. A closing lid, the power
//! button and the Start menu's Sleep still sleep the PC, and the display may turn off, except
//! on a PC with Modern Standby, where Windows does not honour the system's request alone and
//! the display is kept on too ([`aterm_sysprobe::power::windows::requests_of`]).

#![allow(
    non_snake_case,
    reason = "Win32 names are kept verbatim (`CreateEventExW`) to check against the SDK"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use crate::cli::Command;
use crate::model::{Clocks, Mode};
use crate::state;

/// The usage text on Windows.
pub const USAGE: &str = "\
aterm acaffeine — keep this PC awake: no idle sleep of the system while it is on.
A closing lid, the power button and Sleep in the Start menu still sleep it, and
the display may turn off. On a PC with Modern Standby (powercfg /a lists
\"Standby (S0 Low Power Idle)\") Windows does not honour that request alone, so
the display is kept on as well (status: display=on).

USAGE:
    aterm acaffeine [start]
    aterm acaffeine on [1h|8h|24h|forever|<n>h|<n>m]
    aterm acaffeine off
    aterm acaffeine status
    aterm acaffeine quit
    aterm acaffeine serve

    on [span]  Keep the PC awake for the span (forever by default; at most a
               year), then let it sleep. acaffeine=requested until=… says the
               request is saved and a holder runs or was started; `status`
               says whether Windows holds it. From a terminal that ends its
               programs with it (an aterm tab), the holder is started outside
               it, so closing the terminal does not end it.
    off, quit  Stop keeping it awake (acaffeine=off); the holder exits.
    status     acaffeine=on while the holder holds the power request, else off;
               until=<unix seconds>|forever|-; display=on when the display is
               kept on too; holder=running and pid=<n>, or holder=stopped.
    start      Start the holder again when the saved mode is on and none runs
               (after a restart, say: acaffeine=started); otherwise answer as
               status does. The default.
    serve      Hold the power request in this process until the mode is off
               (what `on` starts, with no console).

Nothing starts the holder at sign-in: after a restart, `aterm acaffeine start`
picks a saved span up again, with what is left of it on the clock (one that
ended meanwhile stays off). An elevated `powercfg /requests` lists the request
under SYSTEM as \"aterm aCaffeine is keeping this PC awake\". Exit status: 1 when
a command fails (a running holder that could not be told among them), 2 for a
refused command line. lid, startup and bridge are macOS only.
";

/// The verb's name, as its answers and errors say it.
const VERB: &str = "aterm acaffeine";

/// The power request's reason, which `powercfg /requests` lists it by.
pub const AWAKE_REASON: &str = "aterm aCaffeine is keeping this PC awake";

/// What the holder writes its pid into, beside the state file.
const PID_FILE: &str = "acaffeine.holder.pid";

/// The named events of one state directory's holder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Names {
    /// Exists while a holder runs.
    holder: String,
    /// Exists while the holder holds the power request.
    held: String,
    /// Set to have the holder read the mode again.
    changed: String,
}

impl Names {
    /// The names for the holder of `state_dir`, in this session's namespace: the directory's
    /// path as Windows compares paths (case folded, one separator), hashed (FNV-1a, 64 bits).
    /// PURE.
    pub(crate) fn of(state_dir: &Path) -> Self {
        let folded = state_dir
            .to_string_lossy()
            .replace('/', "\\")
            .to_lowercase();
        let folded = folded.trim_end_matches('\\');
        let hash = folded.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, b| {
            (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        let name = |part: &str| format!("Local\\aterm-acaffeine-{hash:016x}-{part}");
        Self {
            holder: name("holder"),
            held: name("held"),
            changed: name("changed"),
        }
    }
}

#[allow(
    clippy::upper_case_acronyms,
    reason = "the SDK's name, kept to check against it"
)]
type HANDLE = isize;

/// `ERROR_FILE_NOT_FOUND`: no object has the name.
const ERROR_FILE_NOT_FOUND: u32 = 2;
/// `ERROR_PATH_NOT_FOUND`: no namespace has the name's directory.
const ERROR_PATH_NOT_FOUND: u32 = 3;
/// `ERROR_ALREADY_EXISTS`: the named object was there, and the handle is to it.
const ERROR_ALREADY_EXISTS: u32 = 183;
/// `EVENT_QUERY_STATE`: the right to read an event's state.
const EVENT_QUERY_STATE: u32 = 0x0001;
/// `EVENT_MODIFY_STATE`: the right to set an event.
const EVENT_MODIFY_STATE: u32 = 0x0002;
/// `SYNCHRONIZE`: the right to wait on an object; asked for only to learn that it exists.
const SYNCHRONIZE: u32 = 0x0010_0000;
/// What a verb of this user may do with a holder's events: learn that one exists, and set the
/// change event.
const USER_EVENT_RIGHTS: u32 = EVENT_QUERY_STATE | EVENT_MODIFY_STATE | SYNCHRONIZE;
/// What a holder asks of an event it makes or finds made (`CreateEventExW`): to wait on it and
/// set it, which the DACL gives this user ([`USER_EVENT_RIGHTS`]); `CreateEventW` asks all
/// access of one it finds, which that DACL refuses, so a second holder would fail where it
/// must find the first.
const HOLDER_EVENT_ACCESS: u32 = EVENT_MODIFY_STATE | SYNCHRONIZE;
/// `INFINITE`: a wait with no deadline.
const INFINITE: u32 = u32::MAX;
/// `STD_INPUT_HANDLE`, `STD_OUTPUT_HANDLE` and `STD_ERROR_HANDLE`.
const STD_HANDLES: [u32; 3] = [0xFFFF_FFF6, 0xFFFF_FFF5, 0xFFFF_FFF4];
/// `HANDLE_FLAG_INHERIT`: a child started with handle inheritance gets the handle.
const HANDLE_FLAG_INHERIT: u32 = 0x1;
/// `TOKEN_QUERY`: the right to read a token.
const TOKEN_QUERY: u32 = 0x0008;
/// `TOKEN_INFORMATION_CLASS::TokenUser`.
const TOKEN_USER_CLASS: i32 = 1;
/// `SDDL_REVISION_1`.
const SDDL_REVISION_1: u32 = 1;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventExW(
        lpEventAttributes: *const SecurityAttributes,
        lpName: *const u16,
        dwFlags: u32,
        dwDesiredAccess: u32,
    ) -> HANDLE;
    fn OpenEventW(dwDesiredAccess: u32, bInheritHandle: i32, lpName: *const u16) -> HANDLE;
    fn SetEvent(hEvent: HANDLE) -> i32;
    fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: u32) -> u32;
    fn CloseHandle(hObject: HANDLE) -> i32;
    fn GetLastError() -> u32;
    fn GetStdHandle(nStdHandle: u32) -> HANDLE;
    fn SetHandleInformation(hObject: HANDLE, dwMask: u32, dwFlags: u32) -> i32;
    fn GetCurrentProcess() -> HANDLE;
    fn LocalFree(hMem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(ProcessHandle: HANDLE, DesiredAccess: u32, TokenHandle: *mut HANDLE)
    -> i32;
    fn GetTokenInformation(
        TokenHandle: HANDLE,
        TokenInformationClass: i32,
        TokenInformation: *mut core::ffi::c_void,
        TokenInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(Sid: *const core::ffi::c_void, StringSid: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        StringSecurityDescriptor: *const u16,
        StringSDRevision: u32,
        SecurityDescriptor: *mut *mut core::ffi::c_void,
        SecurityDescriptorSize: *mut u32,
    ) -> i32;
}

/// `SECURITY_ATTRIBUTES` (`minwinbase.h`).
#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut core::ffi::c_void,
    inherit: i32,
}

/// The DACL a holder's events are made with, for the user whose SID reads `sid` (`S-1-…`):
/// SYSTEM and the Administrators all access, and the user what a verb asks
/// ([`USER_EVENT_RIGHTS`]), whether the holder runs elevated or not. An elevated token's default
/// DACL gives the Administrators and SYSTEM, not the user's unelevated token, so without this
/// an unelevated `off` could not set an elevated holder's change event. PURE.
pub(crate) fn event_sddl(sid: &str) -> String {
    format!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x{USER_EVENT_RIGHTS:x};;;{sid})")
}

/// The SID of this process's user (`TokenUser`), as text; `None` when it cannot be read.
pub(crate) fn this_user_sid() -> Option<String> {
    let mut token: HANDLE = 0;
    // SAFETY: the current-process pseudo handle, a query right and an out-param.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return None;
    }
    /// The token handle, closed on every way out.
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: the token handle this owns, closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let token = Token(token);
    let mut needed = 0_u32;
    // SAFETY: a size probe: no buffer, and an out-param for the size it needs.
    let _ = unsafe {
        GetTokenInformation(
            token.0,
            TOKEN_USER_CLASS,
            std::ptr::null_mut(),
            0,
            &raw mut needed,
        )
    };
    // `u64`s keep the `TOKEN_USER` (a pointer first) aligned.
    let mut buf = vec![0_u64; (needed as usize).div_ceil(8).max(2)];
    let size = u32::try_from(buf.len() * 8).ok()?;
    // SAFETY: a writable, 8-aligned buffer of `size` bytes and an out-param.
    if unsafe {
        GetTokenInformation(
            token.0,
            TOKEN_USER_CLASS,
            buf.as_mut_ptr().cast(),
            size,
            &raw mut needed,
        )
    } == 0
    {
        return None;
    }
    // SAFETY: the buffer now starts with a `TOKEN_USER`, whose first member is the SID's
    // pointer (into the same buffer).
    let sid = unsafe { buf.as_ptr().cast::<*const core::ffi::c_void>().read() };
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: a SID the buffer holds, and an out-param the call fills with a LocalAlloc'd,
    // NUL-terminated string.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 || text.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated string the call allocated, read up to its NUL and then freed.
    let sid = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        sid
    };
    Some(sid)
}

/// A security descriptor made from SDDL; dropping it frees it.
struct Descriptor(*mut core::ffi::c_void);

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the descriptor the conversion allocated, freed once.
        let _ = unsafe { LocalFree(self.0) };
    }
}

impl Descriptor {
    /// The descriptor `sddl` describes; `None` when Windows does not read it.
    fn from_sddl(sddl: &str) -> Option<Self> {
        let sddl = wide(sddl);
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: a NUL-terminated SDDL string and an out-param the call fills with a
        // LocalAlloc'd descriptor; no size is asked for.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            )
        };
        (ok != 0 && !descriptor.is_null()).then(|| Self(descriptor))
    }

    /// The descriptor every holder event is made with ([`event_sddl`]); `None` when this
    /// user's SID cannot be read, and the event takes the token's default.
    fn for_holder_events() -> Option<Self> {
        Self::from_sddl(&event_sddl(&this_user_sid()?))
    }
}

/// An event handle this process owns; dropping it closes it, and the last handle's close
/// removes a named event.
#[derive(Debug)]
struct Event(HANDLE);

// SAFETY: an event handle is a kernel object any thread may set and wait on.
unsafe impl Send for Event {}
// SAFETY: as above; no method needs exclusive access.
unsafe impl Sync for Event {}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle this owns, closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(Some(0)).collect()
}

impl Event {
    /// The auto-reset event named `name`, made for this user ([`event_sddl`]) or opened:
    /// `(event, was_there)`. `None` when Windows refused it.
    fn create(name: &str) -> Option<(Self, bool)> {
        Self::create_with(name, Descriptor::for_holder_events().as_ref())
    }

    /// [`Event::create`] with `descriptor` (`None`: the token's default DACL).
    fn create_with(name: &str, descriptor: Option<&Descriptor>) -> Option<(Self, bool)> {
        let name = wide(name);
        let attributes = descriptor.map(|descriptor| SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: descriptor.0,
            inherit: 0,
        });
        let attributes = attributes
            .as_ref()
            .map_or(std::ptr::null(), std::ptr::from_ref);
        // SAFETY: security attributes that outlive the call (or none), not inheritable; a
        // NUL-terminated name; no flags (auto reset, not signalled). The last error is read at
        // once, on this thread.
        let (handle, error) = unsafe {
            let handle = CreateEventExW(attributes, name.as_ptr(), 0, HOLDER_EVENT_ACCESS);
            (handle, GetLastError())
        };
        (handle != 0).then(|| (Self(handle), error == ERROR_ALREADY_EXISTS))
    }

    /// The event named `name` with `access`, when one exists.
    fn open(name: &str, access: u32) -> Option<Self> {
        Self::open_or_why(name, access).ok()
    }

    /// The event named `name` with `access`, or the error Windows answered.
    fn open_or_why(name: &str, access: u32) -> Result<Self, u32> {
        let name = wide(name);
        // SAFETY: a NUL-terminated name; the handle is not inheritable. The last error is read
        // at once, on this thread.
        let (handle, error) = unsafe {
            let handle = OpenEventW(access, 0, name.as_ptr());
            (handle, GetLastError())
        };
        if handle == 0 {
            Err(error)
        } else {
            Ok(Self(handle))
        }
    }

    /// Set it: whether Windows did.
    fn set(&self) -> bool {
        // SAFETY: the event this owns.
        unsafe { SetEvent(self.0) != 0 }
    }

    /// Wait until it is set or `millis` are over (`None`: no deadline), taking the set: what
    /// the wait answered (`WAIT_OBJECT_0`, 0, for the set).
    fn wait(&self, millis: Option<u32>) -> u32 {
        // SAFETY: the event this owns.
        unsafe { WaitForSingleObject(self.0, millis.unwrap_or(INFINITE)) }
    }
}

/// Whether an event named `name` exists: some process holds it.
fn exists(name: &str) -> bool {
    Event::open(name, SYNCHRONIZE).is_some()
}

/// Both clocks, read now: the time since boot with sleep counted, which a span's deadline is
/// on, and the wall; `None` when the wall reads before 1970.
pub(crate) fn clocks() -> Option<Clocks> {
    Some(Clocks {
        since_boot: aterm_sysprobe::power::windows::uptime().as_secs(),
        wall: aterm_time::unix_secs()?,
    })
}

/// The mode saved in `file` at `now`: off where nothing was saved or it cannot be read (macOS's
/// mug reads a missing file as forever; here nothing is on that nobody turned on), and off past
/// a span's end. Windows stamps no boot on the file, and its boot clock starts again at a
/// restart, so a span's `ends` counts only while it agrees with the wall-clock `deadline` saved
/// beside it ([`state::read_unstamped`]): after a restart a span is read on the wall clock,
/// off once that end has passed, and never longer than its remainder.
pub(crate) fn saved_mode(file: &Path, now: Clocks) -> Mode {
    state::read_unstamped(file, now).map_or(Mode::Off, |saved| saved.mode.settled(now))
}

/// How long the holder waits before it reads the mode again with nothing told: no deadline
/// while it is on forever, a span's remaining time (the clock is whole seconds, so never under
/// one, and a wait takes at most 2^32 - 2 ms, after which it reads again), and no time once it
/// is off. PURE.
pub(crate) const fn wait_millis(mode: Mode, now: Clocks) -> Option<u32> {
    match mode.settled(now) {
        Mode::Forever => None,
        Mode::Off => Some(0),
        Mode::Until { deadline, .. } => {
            let millis = deadline
                .saturating_sub(now.since_boot)
                .saturating_mul(1_000);
            let cap = (INFINITE - 1) as u64;
            Some(if millis > cap {
                cap as u32
            } else {
                millis as u32
            })
        }
    }
}

/// How a holder's run ended ([`serve_at`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Served {
    /// The mode is off: nothing is held any more.
    Off,
    /// Another holder of this state directory runs: this one held nothing.
    Another,
    /// It could not hold: why.
    Failed(String),
}

/// Run a holder for the state in `state_file` until its mode is off: hold the power request
/// while the mode is on, wait for a change (or the span's end), and let go. `told` hears each
/// take (`true`) and release (`false`) of the request. One holder runs per state directory:
/// a second returns [`Served::Another`] at once.
pub(crate) fn serve_at(state_file: &Path, names: &Names, mut told: impl FnMut(bool)) -> Served {
    let pid_file = state_file.with_file_name(PID_FILE);
    loop {
        let Some((alive, was_there)) = Event::create(&names.holder) else {
            return Served::Failed("Windows refused the holder's event".to_string());
        };
        if was_there {
            return Served::Another;
        }
        let Some((changed, _)) = Event::create(&names.changed) else {
            return Served::Failed("Windows refused the change event".to_string());
        };
        let changed = Arc::new(changed);
        // A span's end is on the clock that runs through a sleep, and a wait's timeout is not:
        // a resume has the loop read the clock again.
        let on_resume = Arc::clone(&changed);
        let _resume = aterm_sysprobe::power::windows::listen(Box::new(move |event| {
            if event == aterm_sysprobe::power::windows::PowerEvent::Resume {
                let _ = on_resume.set();
            }
        }));
        let _ = std::fs::write(&pid_file, format!("{}\n", std::process::id()));
        let mut held = None;
        let failed = loop {
            let Some(now) = clocks() else {
                break Some("the clock reads before 1970".to_string());
            };
            let mode = saved_mode(state_file, now);
            if !mode.is_on(now) {
                break None;
            }
            if held.is_none() {
                let Some(request) =
                    aterm_sysprobe::power::hold(crate::held::SYSTEM_ASSERTION, AWAKE_REASON)
                else {
                    break Some("Windows refused the power request".to_string());
                };
                let Some((mark, _)) = Event::create(&names.held) else {
                    break Some("Windows refused the held event".to_string());
                };
                held = Some((request, mark));
                told(true);
            }
            let _ = changed.wait(wait_millis(mode, now));
        };
        if held.take().is_some() {
            told(false);
        }
        let _ = std::fs::remove_file(&pid_file);
        drop(alive);
        if let Some(why) = failed {
            return Served::Failed(why);
        }
        // An `on` saved after the last read found this holder running and started none: with
        // the holder's event gone, read once more, and hold again if so.
        if !clocks().is_some_and(|now| saved_mode(state_file, now).is_on(now)) {
            return Served::Off;
        }
    }
}

/// What telling the holder to read the mode again found ([`tell_changed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tell {
    /// No holder's change event exists: none runs, or one is starting, which reads the mode
    /// after it makes the event.
    NoHolder,
    /// The running holder was told.
    Told,
    /// A holder runs, but Windows refused this process the right to tell it (one whose events
    /// were made with a DACL that leaves this user out: an elevated holder of a build before
    /// [`event_sddl`]).
    Refused,
}

/// Why a verb fails when the running holder could not be told ([`Tell::Refused`]).
const REFUSED: &str = "the running holder could not be told (one an elevated terminal started?): \
                       run the command from an elevated terminal";

/// Have a running holder read the mode again: whether one was told, there was none, or one
/// runs that this process may not tell.
fn tell_changed(names: &Names) -> Tell {
    match Event::open_or_why(&names.changed, EVENT_MODIFY_STATE) {
        Ok(changed) if changed.set() => Tell::Told,
        Ok(_) => Tell::Refused,
        Err(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => Tell::NoHolder,
        Err(_) => Tell::Refused,
    }
}

/// The running holder's pid, as it wrote it; `None` when none runs or it wrote none.
fn holder_pid(state_file: &Path, names: &Names) -> Option<u32> {
    if !exists(&names.holder) {
        return None;
    }
    std::fs::read_to_string(state_file.with_file_name(PID_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// What the holder found running ([`status_text`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    /// The holder holds the power request.
    pub(crate) held: bool,
    /// A holder runs.
    pub(crate) running: bool,
    /// The pid it wrote.
    pub(crate) pid: Option<u32>,
    /// The request keeps the display on too (a PC with Modern Standby).
    pub(crate) display: bool,
}

/// `status`'s answer, one `key=value` per line: `acaffeine=on` while the request is held and
/// the mode is on, `until=` ([`crate::cli::until_word`]), `display=on` while it is on and keeps
/// the display on too, and the holder. PURE.
pub(crate) fn status_text(mode: Mode, now: Clocks, seen: Seen) -> String {
    let on = seen.held && mode.is_on(now);
    let mode = if on { mode } else { Mode::Off };
    let display = if on && seen.display {
        "display=on\n"
    } else {
        ""
    };
    let holder = match (seen.running, seen.pid) {
        (true, Some(pid)) => format!("holder=running\npid={pid}\n"),
        (true, None) => "holder=running\n".to_string(),
        (false, _) => "holder=stopped\n".to_string(),
    };
    format!(
        "acaffeine={}\nuntil={}\n{display}{holder}",
        if on { "on" } else { "off" },
        crate::cli::until_word(mode, now),
    )
}

/// Where a verb acts: the state file and its holder's names.
struct Here {
    state_file: PathBuf,
    names: Names,
}

fn here() -> Result<Here, String> {
    let paths = crate::paths::Paths::from_env().ok_or("no home directory")?;
    Ok(Here {
        names: Names::of(&paths.state_dir),
        state_file: paths.state_file(),
    })
}

fn now() -> Result<Clocks, String> {
    clocks().ok_or_else(|| "the clock reads before 1970".to_string())
}

fn fail(why: impl std::fmt::Display) -> ExitCode {
    eprintln!("{VERB}: {why}");
    ExitCode::FAILURE
}

fn answer(text: &str) -> ExitCode {
    aterm_uds::stdout::answer_exit(VERB, text, 0)
}

/// Stop this process's standard handles from being inherited by the holder. Its own are NUL,
/// but Windows hands a child every inheritable handle of its parent: a holder that kept the
/// pipe this verb answers on would keep whoever reads that pipe (a script's `$(…)`) waiting
/// for as long as it holds.
fn stop_inheriting_std_handles() {
    for kind in STD_HANDLES {
        // SAFETY: a handle query and a flag write on a handle this process owns; no pointer
        // crosses the boundary.
        unsafe {
            let handle = GetStdHandle(kind);
            if handle != 0 && handle != -1 {
                let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// How a holder was started ([`start_holder_with`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Launched {
    /// As this process's child: this process is in no job, so nothing ends the holder with it.
    Child {
        /// The holder's pid.
        pid: u32,
    },
    /// Outside the job this process runs in ([`aterm_pty::start_outside_job`]).
    OutsideJob(aterm_pty::Detached),
    /// As this process's child in its job, which ends the holder when it ends (an aterm tab's,
    /// when the tab closes): why it could not be started outside.
    InJob {
        /// The holder's pid.
        pid: u32,
        /// Why it could not leave the job.
        why: String,
    },
}

/// The directory a holder runs in: the system's, which nobody removes, so a holder that runs
/// for days never keeps the terminal's directory from being deleted or renamed.
fn holder_directory() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("System32")
}

/// Start a holder: this executable as `acaffeine serve` ([`start_holder_with`]).
fn start_holder() -> Result<Launched, String> {
    let exe = std::env::current_exe().map_err(|e| format!("this executable's path: {e}"))?;
    start_holder_with(&exe, &["acaffeine".into(), "serve".into()], &[])
}

/// Start `exe` with `args` as a holder, with this process's environment and `env` over it,
/// with no console, none of this process's handles and the system directory as its own
/// ([`holder_directory`]); from a process in a job (an aterm tab's shell is in one that ends
/// every process it holds when the tab closes), outside that job, and in it only when it cannot
/// leave ([`Launched::InJob`]). It spawns nothing itself.
pub(crate) fn start_holder_with(
    exe: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
) -> Result<Launched, String> {
    let directory = holder_directory();
    let mut why = None;
    if aterm_pty::in_a_job() {
        let mut environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        environment.extend(env.iter().cloned());
        match aterm_pty::start_outside_job(exe, args, &environment, &directory) {
            Ok(started) => return Ok(Launched::OutsideJob(started)),
            Err(e) => why = Some(e.to_string()),
        }
    }
    let pid = spawn_child(exe, args, env, &directory)?;
    Ok(match why {
        None => Launched::Child { pid },
        Some(why) => Launched::InJob { pid, why },
    })
}

/// Start `exe` with `args` as this process's own child, detached: no console, none of this
/// process's standard handles, `env` over this process's environment, in `directory`.
pub(crate) fn spawn_child(
    exe: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
    directory: &Path,
) -> Result<u32, String> {
    stop_inheriting_std_handles();
    let mut command = aterm_uds::spawnfd::command(exe);
    command
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .current_dir(directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    aterm_uds::spawn::set_gui_image_flags(&mut command);
    command
        .spawn()
        .map(|child| child.id())
        .map_err(|e| format!("{}: {e}", exe.display()))
}

/// What a verb says beside its answer about the holder it started: that it ends with this
/// terminal, when it could not be started outside the terminal's job. PURE.
pub(crate) fn launched_note(launched: &Launched) -> Option<String> {
    match launched {
        Launched::InJob { why, .. } => Some(format!(
            "the holder runs in this terminal's job, so closing the terminal ends it ({why})"
        )),
        Launched::Child { .. } | Launched::OutsideJob(_) => None,
    }
}

/// [`launched_note`] on standard error, where there is one.
fn note_launched(launched: &Launched) {
    if let Some(note) = launched_note(launched) {
        eprintln!("{VERB}: {note}");
    }
}

/// Make sure a holder runs and reads the mode as saved now: the holder started, or `None` when
/// one ran.
///
/// # Errors
///
/// The running holder could not be told ([`Tell::Refused`]), or none could be started.
fn ensure_holder(here: &Here) -> Result<Option<Launched>, String> {
    if tell_changed(&here.names) == Tell::Refused {
        return Err(REFUSED.to_string());
    }
    if exists(&here.names.holder) {
        return Ok(None);
    }
    start_holder().map(Some)
}

fn status(here: &Here, now: Clocks) -> ExitCode {
    let seen = Seen {
        held: exists(&here.names.held),
        running: exists(&here.names.holder),
        pid: holder_pid(&here.state_file, &here.names),
        display: aterm_sysprobe::power::windows::modern_standby() == Some(true),
    };
    answer(&status_text(saved_mode(&here.state_file, now), now, seen))
}

/// The verbs, on Windows.
pub(crate) fn run(command: Command) -> ExitCode {
    let (here, now) = match here().and_then(|here| Ok((here, now()?))) {
        Ok(both) => both,
        Err(why) => return fail(why),
    };
    match command {
        Command::Help => answer(USAGE),
        Command::On { span } => {
            let mode = Mode::activate(span, now);
            if let Err(e) = state::save_at(&here.state_file, mode, now) {
                return fail(format!("{}: {e}", here.state_file.display()));
            }
            match ensure_holder(&here) {
                Ok(launched) => {
                    if let Some(launched) = &launched {
                        note_launched(launched);
                    }
                    answer(&crate::cli::on_line(mode, now))
                }
                Err(why) => fail(format!("request saved, but {why}")),
            }
        }
        Command::Off | Command::Quit { .. } => {
            if let Err(e) = state::save_at(&here.state_file, Mode::Off, now) {
                return fail(format!("{}: {e}", here.state_file.display()));
            }
            match tell_changed(&here.names) {
                Tell::Refused => fail(format!("mode saved off, but {REFUSED}")),
                Tell::Told | Tell::NoHolder => answer("acaffeine=off\n"),
            }
        }
        Command::Start => {
            if !saved_mode(&here.state_file, now).is_on(now) || exists(&here.names.holder) {
                return status(&here, now);
            }
            match start_holder() {
                Ok(launched) => {
                    note_launched(&launched);
                    answer("acaffeine=started\n")
                }
                Err(why) => fail(format!("no holder started: {why}")),
            }
        }
        Command::Status => status(&here, now),
        Command::Serve => match serve_at(&here.state_file, &here.names, |_| {}) {
            Served::Off | Served::Another => ExitCode::SUCCESS,
            Served::Failed(why) => fail(why),
        },
        Command::Lid(_) => fail("lid is macOS only"),
        Command::Startup(_) => fail("startup is macOS only"),
        Command::Bridge => fail("bridge is macOS only"),
    }
}

#[cfg(test)]
#[path = "win_tests.rs"]
mod tests;
