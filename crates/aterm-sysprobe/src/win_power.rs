// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows' power readers and its keep-awake request (`crate::power::windows`): what powers the
//! machine and the battery (`GetSystemPowerStatus`), when the kernel booted
//! (`SystemTimeOfDayInformation`), the clock that stops while the machine is suspended
//! (`QueryUnbiasedInterruptTime`), a suspend and a resume as the power manager announces them
//! to a callback (`PowerRegisterSuspendResumeNotification` with `DEVICE_NOTIFY_CALLBACK`: no
//! window and no message loop), whether the PC has Modern Standby (`CallNtPowerInformation`'s
//! power capabilities), and a power request of this process's own (`PowerCreateRequest`),
//! which is what [`crate::power::hold`] holds here.
//!
//! What the calls return is decoded by the functions at the top, compiled and tested on every
//! host. `powrprof.dll` is loaded when a listener or the capabilities (a hold reads them) are
//! first asked for, never imported: a process that does neither maps none of it. Nothing here
//! holds a timer or a thread: a
//! reader reads when asked, and the listener's callback runs on a thread of the power manager's.

use std::time::Duration;

use crate::situation::{BatteryReading, PowerReading, PowerSource};

/// `SYSTEM_POWER_STATUS` (`winbase.h`), as `GetSystemPowerStatus` fills it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemPowerStatus {
    /// `ACLineStatus`: 0 offline, 1 online, 255 unknown.
    pub ac_line: u8,
    /// `BatteryFlag`: bits [`BATTERY_CHARGING`] and [`BATTERY_NONE`]; 255 unknown.
    pub battery_flag: u8,
    /// `BatteryLifePercent`: 0–100, or 255 unknown.
    pub battery_percent: u8,
    /// `SystemStatusFlag`: 1 while battery saver is on.
    pub system_status: u8,
    /// `BatteryLifeTime`: seconds of charge left, or `u32::MAX` unknown.
    pub battery_life_secs: u32,
    /// `BatteryFullLifeTime`: seconds a full charge lasts, or `u32::MAX` unknown.
    pub battery_full_life_secs: u32,
}
const _: () = assert!(std::mem::size_of::<SystemPowerStatus>() == 12);

/// `BatteryFlag`'s charging bit.
pub const BATTERY_CHARGING: u8 = 8;
/// `BatteryFlag`'s "no system battery" bit.
pub const BATTERY_NONE: u8 = 128;
/// `ACLineStatus`, `BatteryFlag` and `BatteryLifePercent`'s "unknown".
pub const STATUS_UNKNOWN: u8 = 255;

/// The power reading `status` names: AC when the line is online, the battery when it is
/// offline, and `None` when Windows does not know which (`ACLineStatus` 255, or a value it
/// does not define). The battery is read only where there is one and its charge is known. A
/// UPS a desktop reports as its battery reads as a battery, as Windows shows it. PURE.
#[must_use]
pub fn reading_of(status: &SystemPowerStatus) -> Option<PowerReading> {
    let source = match status.ac_line {
        0 => PowerSource::Battery,
        1 => PowerSource::Ac,
        _ => return None,
    };
    let known = status.battery_flag != STATUS_UNKNOWN
        && status.battery_flag & BATTERY_NONE == 0
        && status.battery_percent != STATUS_UNKNOWN;
    Some(PowerReading {
        source,
        battery: known.then(|| BatteryReading {
            pct: status.battery_percent.min(100),
            charging: status.battery_flag & BATTERY_CHARGING != 0,
        }),
        adapter_w: None,
    })
}

/// What the power manager announced to a suspend and resume callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerEvent {
    /// `PBT_APMSUSPEND`: the machine is about to suspend (sleep, hibernate, or a modern
    /// standby's deep phase). The callback has about two seconds; nothing is acknowledged.
    Suspend,
    /// `PBT_APMRESUMEAUTOMATIC`: the machine resumed. It is sent at every resume, before the
    /// `PBT_APMRESUMESUSPEND` that follows only when a person woke it.
    Resume,
}

/// `PBT_APMSUSPEND` (`winuser.h`).
pub const PBT_APMSUSPEND: u32 = 0x0004;
/// `PBT_APMRESUMESUSPEND`: a person woke the machine; it follows [`PBT_APMRESUMEAUTOMATIC`].
pub const PBT_APMRESUMESUSPEND: u32 = 0x0007;
/// `PBT_APMRESUMEAUTOMATIC`.
pub const PBT_APMRESUMEAUTOMATIC: u32 = 0x0012;

/// The event a callback's `Type` names: one per suspend and one per resume. The second resume
/// word ([`PBT_APMRESUMESUSPEND`]) and every other word (a power-setting change) name none, so
/// one resume is never heard twice. PURE.
#[must_use]
pub const fn event_of(kind: u32) -> Option<PowerEvent> {
    match kind {
        PBT_APMSUSPEND => Some(PowerEvent::Suspend),
        PBT_APMRESUMEAUTOMATIC => Some(PowerEvent::Resume),
        _ => None,
    }
}

/// 100 ns intervals from 1601-01-01 (a `FILETIME`'s epoch) to 1970-01-01.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

/// A `FILETIME` (100 ns since 1601, UTC) as unix milliseconds; `None` for one at or before
/// 1970, which no boot of this kernel is (a zeroed answer reads as none). PURE.
#[must_use]
pub const fn filetime_unix_ms(filetime: i64) -> Option<i64> {
    if filetime <= FILETIME_UNIX_EPOCH {
        return None;
    }
    Some((filetime - FILETIME_UNIX_EPOCH) / 10_000)
}

/// An interrupt-time count (100 ns units) as a duration. PURE.
#[must_use]
pub const fn interrupt_time(count: u64) -> Duration {
    Duration::from_nanos(count.saturating_mul(100))
}

/// A power request's kind (`POWER_REQUEST_TYPE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// `PowerRequestDisplayRequired`: the display stays on.
    Display = 0,
    /// `PowerRequestSystemRequired`: the system does not sleep on its idle timer. It does not
    /// keep a closing lid, the power button or the Start menu's Sleep from sleeping it, and
    /// Windows documents that a PC capable of Modern Standby does not honour it.
    System = 1,
    /// `PowerRequestExecutionRequired`: the requesting process keeps running instead of being
    /// suspended by the system's process lifetime management, as Windows asks of a program on
    /// a Modern Standby PC in place of [`Request::System`]. It covers that process alone (no
    /// other program it is meant to keep running), and on battery Windows may bound it. On a
    /// PC with no Modern Standby Windows treats it as [`Request::System`].
    Execution = 3,
}

/// What a hold that keeps the system from its idle sleep sets.
const SYSTEM: &[Request] = &[Request::System, Request::Execution];
/// What a hold that also keeps the display on sets.
const SYSTEM_AND_DISPLAY: &[Request] = &[Request::System, Request::Execution, Request::Display];

/// The power requests a [`crate::power::hold`] of `kind` sets on Windows, by the macOS
/// assertion types its callers name: the two that keep the system from sleeping set
/// [`Request::System`] and [`Request::Execution`]; the one that keeps the display on sets
/// [`Request::Display`] and, as on macOS, the system with it. On a PC with Modern Standby
/// (`modern_standby`, [`modern_standby`]) the system's two set the display too: Windows
/// documents that it does not honour a system-required request there, and the display
/// turning off is what lets such a PC go into its standby, where other programs (the ones a
/// keep-awake is for) are suspended. Any other word holds nothing. PURE.
#[must_use]
pub fn requests_of(kind: &str, modern_standby: bool) -> Option<&'static [Request]> {
    match kind {
        crate::power::PREVENT_SYSTEM_SLEEP | "PreventUserIdleSystemSleep" if modern_standby => {
            Some(SYSTEM_AND_DISPLAY)
        }
        crate::power::PREVENT_SYSTEM_SLEEP | "PreventUserIdleSystemSleep" => Some(SYSTEM),
        "PreventUserIdleDisplaySleep" => Some(SYSTEM_AND_DISPLAY),
        _ => None,
    }
}

/// `SYSTEM_POWER_CAPABILITIES` (`winnt.h`), as `CallNtPowerInformation(SystemPowerCapabilities)`
/// fills it: 76 bytes, of which only [`PowerCapabilities::aoac`] and
/// [`PowerCapabilities::s3`] are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerCapabilities(pub [u8; POWER_CAPABILITIES_LEN]);

/// `sizeof(SYSTEM_POWER_CAPABILITIES)`: 30 one-byte fields, `BATTERY_REPORTING_SCALE[3]` (24
/// bytes) and five `SYSTEM_POWER_STATE`s.
pub const POWER_CAPABILITIES_LEN: usize = 76;
/// `SYSTEM_POWER_CAPABILITIES.SystemS3`'s offset: after the three button and lid flags and S1, S2.
const SYSTEM_S3_AT: usize = 5;
/// `SYSTEM_POWER_CAPABILITIES.AoAc`'s offset: after `PowerButtonPresent` … `WakeAlarmPresent`.
const AOAC_AT: usize = 20;

impl PowerCapabilities {
    /// `AoAc`: the PC supports Modern Standby (always on, always connected; `powercfg /a`
    /// lists "Standby (S0 Low Power Idle)"). PURE.
    #[must_use]
    pub const fn aoac(&self) -> bool {
        self.0[AOAC_AT] != 0
    }

    /// `SystemS3`: the PC has the classic sleep, which Windows disables where Modern Standby
    /// is supported. PURE.
    #[must_use]
    pub const fn s3(&self) -> bool {
        self.0[SYSTEM_S3_AT] != 0
    }
}

/// The last suspend and the last resume a listener of this process heard ([`listen`]), unix
/// ms; `None` for one not heard since this process started listening.
#[must_use]
pub fn last_sleep() -> (Option<i64>, Option<i64>) {
    #[cfg(windows)]
    {
        ffi::last_sleep()
    }
    #[cfg(not(windows))]
    {
        (None, None)
    }
}

/// Whether this PC has Modern Standby ([`PowerCapabilities::aoac`]), read once a
/// process; `None` when the power manager did not say, and off Windows.
#[must_use]
pub fn modern_standby() -> Option<bool> {
    #[cfg(windows)]
    {
        static READ: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
        *READ.get_or_init(|| ffi::power_capabilities().map(|caps| caps.aoac()))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
pub use ffi::{
    Held, Listening, boot_unix_ms, hold, listen, power_capabilities, power_reading, unbiased_time,
    uptime,
};

#[cfg(windows)]
mod ffi {
    #![allow(
        non_snake_case,
        clippy::upper_case_acronyms,
        reason = "Win32 names are kept verbatim (`HANDLE`, `PowerSetRequest`) to check against the SDK"
    )]

    use std::ffi::{CStr, c_void};
    use std::panic::AssertUnwindSafe;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::Duration;

    use super::{
        POWER_CAPABILITIES_LEN, PowerCapabilities, PowerEvent, Request, SystemPowerStatus,
        event_of, filetime_unix_ms, interrupt_time, reading_of, requests_of,
    };
    use crate::situation::PowerReading;

    type BOOL = i32;
    type HANDLE = isize;
    /// Any export, before it is given its signature.
    type Export = unsafe extern "system" fn() -> isize;

    /// `INVALID_HANDLE_VALUE`, which `PowerCreateRequest` answers a failure with.
    const INVALID_HANDLE_VALUE: HANDLE = -1;
    /// `LOAD_LIBRARY_SEARCH_SYSTEM32`: the system directory only.
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
    /// `SystemTimeOfDayInformation`.
    const SYSTEM_TIME_OF_DAY_INFORMATION: i32 = 3;
    /// `POWER_REQUEST_CONTEXT_VERSION`.
    const POWER_REQUEST_CONTEXT_VERSION: u32 = 0;
    /// `POWER_REQUEST_CONTEXT_SIMPLE_STRING`: the reason is one string.
    const POWER_REQUEST_CONTEXT_SIMPLE_STRING: u32 = 0x1;
    /// `DEVICE_NOTIFY_CALLBACK`: the recipient is a [`SubscribeParameters`], not a window.
    const DEVICE_NOTIFY_CALLBACK: u32 = 2;
    /// `POWER_INFORMATION_LEVEL::SystemPowerCapabilities`.
    const SYSTEM_POWER_CAPABILITIES: i32 = 4;

    /// `REASON_CONTEXT` (`minwinbase.h`): the simple string lies over the detailed reason's
    /// module handle, the union's first member.
    #[repr(C)]
    struct ReasonContext {
        version: u32,
        flags: u32,
        simple_reason: *const u16,
        localized_id: u32,
        string_count: u32,
        strings: *const c_void,
    }
    const _: () =
        assert!(std::mem::size_of::<ReasonContext>() == 8 + 2 * std::mem::size_of::<usize>() + 8);

    /// `SYSTEM_TIMEOFDAY_INFORMATION` (ntdll), as far as its boot time and the two fields after
    /// it, which every Windows since XP answers.
    #[repr(C)]
    #[derive(Default)]
    struct TimeOfDay {
        boot_time: i64,
        current_time: i64,
        time_zone_bias: i64,
        time_zone_id: u32,
        reserved: u32,
        boot_time_bias: u64,
        sleep_time_bias: u64,
    }
    const _: () = assert!(std::mem::size_of::<TimeOfDay>() == 48);

    /// `DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS` (`powrprof.h`).
    #[repr(C)]
    struct SubscribeParameters {
        callback: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> u32,
        context: *mut c_void,
    }

    #[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
    unsafe extern "system" {
        fn GetSystemPowerStatus(lpSystemPowerStatus: *mut SystemPowerStatus) -> BOOL;
        fn QueryUnbiasedInterruptTime(UnbiasedTime: *mut u64) -> BOOL;
        fn GetTickCount64() -> u64;
        fn PowerCreateRequest(Context: *const ReasonContext) -> HANDLE;
        fn PowerSetRequest(PowerRequest: HANDLE, RequestType: i32) -> BOOL;
        fn PowerClearRequest(PowerRequest: HANDLE, RequestType: i32) -> BOOL;
        fn CloseHandle(hObject: HANDLE) -> BOOL;
        fn LoadLibraryExW(lpLibFileName: *const u16, hFile: HANDLE, dwFlags: u32) -> HANDLE;
        fn GetProcAddress(hModule: HANDLE, lpProcName: *const u8) -> Option<Export>;
    }

    #[link(name = "ntdll.dll", kind = "raw-dylib", modifiers = "+verbatim")]
    unsafe extern "system" {
        fn NtQuerySystemInformation(
            SystemInformationClass: i32,
            SystemInformation: *mut c_void,
            SystemInformationLength: u32,
            ReturnLength: *mut u32,
        ) -> i32;
    }

    /// One read of what powers the machine and its battery ([`reading_of`]); `None` when the
    /// call fails or Windows does not know the source.
    #[must_use]
    pub fn power_reading() -> Option<PowerReading> {
        let mut status = SystemPowerStatus::default();
        // SAFETY: one writable `SYSTEM_POWER_STATUS` that outlives the call.
        if unsafe { GetSystemPowerStatus(&raw mut status) } == 0 {
            return None;
        }
        reading_of(&status)
    }

    /// How long since this machine booted, the time it was suspended counted (`GetTickCount64`):
    /// the clock a deadline that a sleep must not put off is kept on.
    #[must_use]
    pub fn uptime() -> Duration {
        // SAFETY: takes nothing, returns a count of milliseconds.
        Duration::from_millis(unsafe { GetTickCount64() })
    }

    /// How long this machine has run since it booted, not counting the time it was suspended
    /// (`QueryUnbiasedInterruptTime`): the clock a sleep's length is read against, since std's
    /// `Instant` (the performance counter) keeps counting through a suspend here.
    #[must_use]
    pub fn unbiased_time() -> Option<Duration> {
        let mut count = 0_u64;
        // SAFETY: one writable 64-bit count that outlives the call.
        (unsafe { QueryUnbiasedInterruptTime(&raw mut count) } != 0).then(|| interrupt_time(count))
    }

    /// When the kernel booted, unix ms (`SystemTimeOfDayInformation`'s `BootTime`, the one
    /// figure Task Manager's "Up time" is counted from). A shutdown with Fast Startup on
    /// hibernates the kernel, so the boot it names is the last full one.
    #[must_use]
    pub fn boot_unix_ms() -> Option<i64> {
        let mut info = TimeOfDay::default();
        let mut returned = 0_u32;
        // SAFETY: a writable buffer of the size passed, and a writable returned length.
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_TIME_OF_DAY_INFORMATION,
                (&raw mut info).cast(),
                std::mem::size_of::<TimeOfDay>() as u32,
                &raw mut returned,
            )
        };
        if status < 0 || (returned as usize) < std::mem::size_of::<i64>() {
            return None;
        }
        filetime_unix_ms(info.boot_time)
    }

    /// A power request of this process's own with its kinds set; dropping it clears each and
    /// closes it. The request ends with the process whatever becomes of this.
    #[derive(Debug)]
    pub struct Held {
        request: HANDLE,
        set: &'static [Request],
    }

    impl Drop for Held {
        fn drop(&mut self) {
            for kind in self.set {
                // SAFETY: the request this owns, and a kind it set.
                let _ = unsafe { PowerClearRequest(self.request, *kind as i32) };
            }
            // SAFETY: the handle this owns, closed once.
            let _ = unsafe { CloseHandle(self.request) };
        }
    }

    /// A power request named `name` (what `powercfg /requests` lists it by) holding what a
    /// [`crate::power::hold`] of `kind` asks for ([`requests_of`]); `None` for a kind that maps
    /// to none, a name with a NUL, or when the power manager refused the request or any of its
    /// kinds (nothing stays held then).
    #[must_use]
    pub fn hold(kind: &str, name: &str) -> Option<Held> {
        let set = requests_of(kind, super::modern_standby() == Some(true))?;
        if name.contains('\0') {
            return None;
        }
        let reason: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let context = ReasonContext {
            version: POWER_REQUEST_CONTEXT_VERSION,
            flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            simple_reason: reason.as_ptr(),
            localized_id: 0,
            string_count: 0,
            strings: std::ptr::null(),
        };
        // SAFETY: a `REASON_CONTEXT` whose simple string is NUL-terminated and outlives the
        // call, which copies it.
        let request = unsafe { PowerCreateRequest(&raw const context) };
        if request == 0 || request == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut held = Held { request, set: &[] };
        for (index, kind) in set.iter().enumerate() {
            // SAFETY: the request just made, and a kind `POWER_REQUEST_TYPE` defines.
            if unsafe { PowerSetRequest(request, *kind as i32) } == 0 {
                return None;
            }
            held.set = &set[..=index];
        }
        Some(held)
    }

    /// The last suspend and resume heard, unix ms; 0 for none.
    static LAST_SUSPEND_MS: AtomicI64 = AtomicI64::new(0);
    static LAST_RESUME_MS: AtomicI64 = AtomicI64::new(0);

    pub(super) fn last_sleep() -> (Option<i64>, Option<i64>) {
        let recorded = |cell: &AtomicI64| Some(cell.load(Ordering::Relaxed)).filter(|ms| *ms > 0);
        (recorded(&LAST_SUSPEND_MS), recorded(&LAST_RESUME_MS))
    }

    fn unix_ms_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }

    type Register =
        unsafe extern "system" fn(u32, *const SubscribeParameters, *mut *mut c_void) -> u32;
    type Unregister = unsafe extern "system" fn(*mut c_void) -> u32;
    /// `CallNtPowerInformation`.
    type Information = unsafe extern "system" fn(i32, *const c_void, u32, *mut c_void, u32) -> i32;

    /// `powrprof.dll`'s entry points.
    struct PowrProf {
        /// `PowerRegisterSuspendResumeNotification`.
        register: Register,
        /// `PowerUnregisterSuspendResumeNotification`.
        unregister: Unregister,
        /// `CallNtPowerInformation`.
        information: Information,
    }

    /// What this PC's power manager says it can do (`SystemPowerCapabilities`); `None` when
    /// `powrprof.dll` or the call did not answer.
    #[must_use]
    pub fn power_capabilities() -> Option<PowerCapabilities> {
        let powrprof = powrprof()?;
        let mut caps = PowerCapabilities([0; POWER_CAPABILITIES_LEN]);
        // SAFETY: no input, and a writable output of the size passed, which the call fills.
        let status = unsafe {
            (powrprof.information)(
                SYSTEM_POWER_CAPABILITIES,
                std::ptr::null(),
                0,
                caps.0.as_mut_ptr().cast(),
                POWER_CAPABILITIES_LEN as u32,
            )
        };
        (status == 0).then_some(caps)
    }

    fn load() -> Option<PowrProf> {
        let name: Vec<u16> = "powrprof.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: `name` is a NUL-terminated wide string; the file handle is reserved (0).
        let module = unsafe { LoadLibraryExW(name.as_ptr(), 0, LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if module == 0 {
            return None;
        }
        let export = |name: &CStr| {
            // SAFETY: a live module handle (never freed) and a NUL-terminated export name.
            unsafe { GetProcAddress(module, name.as_ptr().cast()) }
        };
        // SAFETY (each transmute): the export has the signature `powrprof.h` declares for its
        // name, and function pointers of one ABI have one representation.
        Some(PowrProf {
            register: unsafe {
                std::mem::transmute::<Export, Register>(export(
                    c"PowerRegisterSuspendResumeNotification",
                )?)
            },
            unregister: unsafe {
                std::mem::transmute::<Export, Unregister>(export(
                    c"PowerUnregisterSuspendResumeNotification",
                )?)
            },
            information: unsafe {
                std::mem::transmute::<Export, Information>(export(c"CallNtPowerInformation")?)
            },
        })
    }

    fn powrprof() -> Option<&'static PowrProf> {
        static POWRPROF: OnceLock<Option<PowrProf>> = OnceLock::new();
        POWRPROF.get_or_init(load).as_ref()
    }

    /// What a registration's callback reaches through its context.
    struct Listener {
        handler: Box<dyn Fn(PowerEvent) + Send + Sync>,
    }

    impl Listener {
        /// Hand `kind`'s event, if it names one, to the handler; a handler that panics is
        /// caught here, since nothing may unwind into the power manager's thread.
        fn hear(&self, kind: u32) {
            if let Some(event) = event_of(kind) {
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| (self.handler)(event)));
            }
        }
    }

    /// The power manager's callback: the event noted for [`last_sleep`], then heard.
    unsafe extern "system" fn announced(
        context: *mut c_void,
        kind: u32,
        _setting: *mut c_void,
    ) -> u32 {
        match event_of(kind) {
            Some(PowerEvent::Suspend) => LAST_SUSPEND_MS.store(unix_ms_now(), Ordering::Relaxed),
            Some(PowerEvent::Resume) => LAST_RESUME_MS.store(unix_ms_now(), Ordering::Relaxed),
            None => {}
        }
        // SAFETY: `context` is the `Listener` its `Listening` keeps alive until the
        // registration is ended, which waits for a callback in flight.
        unsafe { &*context.cast::<Listener>() }.hear(kind);
        0
    }

    /// A suspend and resume listener [`listen`] registered: it hears until this is dropped,
    /// which ends the registration (waiting for a callback in flight) and then frees what the
    /// callback reads. Never drop it from its own handler.
    pub struct Listening {
        registration: *mut c_void,
        unregister: Unregister,
        listener: *mut Listener,
        /// The recipient the registration was made with, kept until it ends.
        _parameters: Box<SubscribeParameters>,
    }

    // SAFETY: the registration handle is the power manager's, usable from any thread; the
    // listener is reached only through shared references, and its handler is `Send + Sync`.
    unsafe impl Send for Listening {}
    // SAFETY: as above; no method mutates through `&self`.
    unsafe impl Sync for Listening {}

    impl std::fmt::Debug for Listening {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Listening").finish_non_exhaustive()
        }
    }

    impl Listening {
        /// Hand the handler what a callback of `kind` (a `PBT_*` word) would, on this thread:
        /// the callback's own decode and catch, without the power manager and without a note
        /// for [`last_sleep`]. A test's hand; a real suspend cannot be asked for.
        #[cfg(any(test, debug_assertions))]
        pub fn deliver(&self, kind: u32) {
            // SAFETY: the listener lives until `self` is dropped.
            unsafe { &*self.listener }.hear(kind);
        }
    }

    impl Drop for Listening {
        fn drop(&mut self) {
            // SAFETY: the registration this owns, ended once; the call returns only once no
            // callback of it runs.
            let _ = unsafe { (self.unregister)(self.registration) };
            // SAFETY: the box `listen` leaked for this registration, which no callback reads
            // any more.
            drop(unsafe { Box::from_raw(self.listener) });
        }
    }

    /// Hear this machine's suspends and resumes: `handler` runs on a thread of the power
    /// manager's at each ([`PowerEvent`]), until the returned [`Listening`] is dropped. A
    /// suspend gives its callbacks about two seconds, so a handler notes and returns. `None`
    /// when `powrprof.dll` has no such registration or it was refused.
    #[must_use]
    pub fn listen(handler: Box<dyn Fn(PowerEvent) + Send + Sync>) -> Option<Listening> {
        let powrprof = powrprof()?;
        let listener = Box::into_raw(Box::new(Listener { handler }));
        let parameters = Box::new(SubscribeParameters {
            callback: announced,
            context: listener.cast(),
        });
        let mut registration = std::ptr::null_mut();
        // SAFETY: the recipient is a `DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS` that lives as long as
        // the registration, as its context does; the out-pointer is this frame's.
        let status = unsafe {
            (powrprof.register)(
                DEVICE_NOTIFY_CALLBACK,
                &raw const *parameters,
                &raw mut registration,
            )
        };
        if status != 0 || registration.is_null() {
            // SAFETY: the box leaked above, which no registration names.
            drop(unsafe { Box::from_raw(listener) });
            return None;
        }
        Some(Listening {
            registration,
            unregister: powrprof.unregister,
            listener,
            _parameters: parameters,
        })
    }
}

#[cfg(test)]
#[path = "win_power_tests.rs"]
mod tests;
