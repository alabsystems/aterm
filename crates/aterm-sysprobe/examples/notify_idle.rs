// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M-NOTIFY (stats deck A4, `docs/DESIGN-stats-deck-instruments-2026-11-06.md` §16 and §10):
//! every notification A4's situation and heat readers need, registered in one idle process on a
//! run loop, so its wakeups at rest can be counted from outside and each event it hears is
//! logged with when it arrived.
//!
//! `notify_idle listen --out F` registers, on its main thread's run loop and with no timer:
//!
//! - the thermal pressure level (`com.apple.system.thermalpressurelevel`), the power source
//!   (`com.apple.system.powersources.source`, IOKit's `kIOPSNotifyPowerSource`: AC or battery),
//!   the battery's charge (`com.apple.system.powersources.percent`, `kIOPSNotifyPercentChange`:
//!   each change of the charge, which the battery bar and `battery<=20` read; the source key
//!   alone fires only on a switch) and Low Power Mode (`com.apple.system.lowpowermode`), each as a mach-port notify
//!   registration on one port (`NOTIFY_REUSE`), beside a check token for each (what a reading's
//!   `notify_check` reads; a check token wakes nothing);
//! - the power manager's root domain (`IOPMrootDomain`) general interest, which carries its
//!   clamshell message (`kIOPMMessageClamshellStateChange`);
//! - the framebuffers (`IOMobileFramebufferShim`): first-match and terminated matching
//!   notifications (the docked glyph's reader), and with `--fb-interest` each one's general
//!   interest, which carried 304 graphics messages in 5 s once on 2026-11-06;
//! - sleep and wake, through aterm's own listener (`aterm_uds::power::listen`), on its thread.
//!
//! Each message is a line of `F` with the monotonic and wall time it arrived, the state read in
//! the callback and what that read cost. For an AC change it adds the battery's own
//! `UpdateTime` (seconds), so the line carries the delivery latency against it; for a lid change
//! it adds the time since the last sleep or wake message. `--control-sleep-ms N` adds a thread
//! that sleeps N ms in a loop: the negative control, whose timer wakes the counters must see.
//!
//! `notify_idle wakes PID` prints one reading of PID's wake counters (`proc_pid_rusage`'s
//! interrupt and package-idle wakeups, CPU time, and `PROC_PIDTASKINFO`'s context switches and
//! messages received); `notify_idle watch PID --every-s N --secs S --out F` writes them as a
//! series. A timer that fires wakes a thread from interrupt context, which is what
//! `ri_interrupt_wkups` counts; a notify or IOKit message wakes it from the sender's thread and
//! is a context switch and a message received, not an interrupt wakeup.

#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("listen") => listen(&args),
        Some("wakes") => wakes(&args),
        Some("watch") => watch(&args),
        _ => {
            eprintln!("usage: notify_idle listen|wakes|watch ... (see the file's header)");
            2
        }
    };
    std::process::exit(code);
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(not(target_os = "macos"))]
fn listen(_args: &[String]) -> i32 {
    eprintln!("notify_idle: macOS only");
    1
}

#[cfg(not(target_os = "macos"))]
fn wakes(_args: &[String]) -> i32 {
    eprintln!("notify_idle: macOS only");
    1
}

#[cfg(not(target_os = "macos"))]
fn watch(_args: &[String]) -> i32 {
    eprintln!("notify_idle: macOS only");
    1
}

/// One process's wake counters, read from outside it.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Default)]
struct Wakes {
    interrupt: u64,
    pkg_idle: u64,
    user_ns: u64,
    system_ns: u64,
    csw: i64,
    msgs_received: i64,
    syscalls_mach: i64,
    syscalls_unix: i64,
    threads: i64,
}

#[cfg(target_os = "macos")]
fn read_wakes(pid: i32) -> Option<Wakes> {
    let mut ri = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `ri` is a writable `rusage_info_v4`, which the V4 flavor fills.
    let rc = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, ri.as_mut_ptr().cast()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: the kernel filled it (rc 0); it was zeroed before, so every byte is initialised.
    let ri = unsafe { ri.assume_init() };
    let (numer, denom) = aterm_sysprobe::procs::kernel_timebase().unwrap_or((1, 1));
    let ns = |t| aterm_sysprobe::procs::ticks_to_ns(t, numer, denom);
    let mut ti = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    /// `PROC_PIDTASKINFO` (`sys/proc_info.h`).
    const PROC_PIDTASKINFO: i32 = 4;
    // SAFETY: `ti` is a writable `proc_taskinfo` of `size` bytes.
    let got = unsafe { libc::proc_pidinfo(pid, PROC_PIDTASKINFO, 0, ti.as_mut_ptr().cast(), size) };
    // SAFETY: zeroed, then filled where `got` is its size.
    let ti = (got == size).then(|| unsafe { ti.assume_init() });
    Some(Wakes {
        interrupt: ri.ri_interrupt_wkups,
        pkg_idle: ri.ri_pkg_idle_wkups,
        user_ns: ns(ri.ri_user_time),
        system_ns: ns(ri.ri_system_time),
        csw: ti.map_or(-1, |t| i64::from(t.pti_csw)),
        msgs_received: ti.map_or(-1, |t| i64::from(t.pti_messages_received)),
        syscalls_mach: ti.map_or(-1, |t| i64::from(t.pti_syscalls_mach)),
        syscalls_unix: ti.map_or(-1, |t| i64::from(t.pti_syscalls_unix)),
        threads: ti.map_or(-1, |t| i64::from(t.pti_threadnum)),
    })
}

#[cfg(target_os = "macos")]
fn wakes_line(pid: i32, t: f64) -> String {
    match read_wakes(pid) {
        Some(w) => format!(
            "{{\"unix_ms\":{},\"t\":{t:.1},\"pid\":{pid},\"interrupt_wkups\":{},\"pkg_idle_wkups\":{},\"user_ns\":{},\"system_ns\":{},\"csw\":{},\"msgs_received\":{},\"syscalls_mach\":{},\"syscalls_unix\":{},\"threads\":{}}}",
            unix_ms(),
            w.interrupt,
            w.pkg_idle,
            w.user_ns,
            w.system_ns,
            w.csw,
            w.msgs_received,
            w.syscalls_mach,
            w.syscalls_unix,
            w.threads
        ),
        None => format!(
            "{{\"unix_ms\":{},\"t\":{t:.1},\"pid\":{pid},\"gone\":true}}",
            unix_ms()
        ),
    }
}

#[cfg(target_os = "macos")]
fn wakes(args: &[String]) -> i32 {
    let Some(pid) = args.get(1).and_then(|p| p.parse::<i32>().ok()) else {
        eprintln!("wakes: PID");
        return 2;
    };
    println!("{}", wakes_line(pid, 0.0));
    0
}

#[cfg(target_os = "macos")]
fn watch(args: &[String]) -> i32 {
    let Some(pid) = args.get(1).and_then(|p| p.parse::<i32>().ok()) else {
        eprintln!("watch: PID");
        return 2;
    };
    let every = flag(args, "--every-s")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60)
        .max(1);
    let secs = flag(args, "--secs")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(600);
    let Some(path) = flag(args, "--out") else {
        eprintln!("watch: --out FILE");
        return 2;
    };
    let Ok(mut out) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        eprintln!("watch: cannot write {path}");
        return 1;
    };
    let t0 = Instant::now();
    let mut k = 0u64;
    loop {
        let line = wakes_line(pid, t0.elapsed().as_secs_f64());
        let gone = line.contains("\"gone\"");
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
        if gone {
            return 0;
        }
        k += 1;
        let due = t0 + Duration::from_secs(k * every);
        if due > t0 + Duration::from_secs(secs) {
            return 0;
        }
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
}

/// The log every callback writes to, and the instant the process started.
static LOG: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
static T0: OnceLock<Instant> = OnceLock::new();
/// The last sleep or wake message, and the last clamshell message: (wall ms, what).
static LAST_POWER: Mutex<Option<(i64, &'static str)>> = Mutex::new(None);
static LAST_LID: Mutex<Option<i64>> = Mutex::new(None);

fn mono_ms() -> f64 {
    T0.get().map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0)
}

fn log(line: &str) {
    if let Some(f) = LOG.get() {
        let mut f = f.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
}

#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals, non_snake_case)]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub type CFIndex = isize;
    pub type CFTypeRef = *const c_void;
    pub type CFStringRef = *const c_void;
    pub type CFAllocatorRef = *const c_void;
    pub type CFRunLoopRef = *mut c_void;
    pub type CFRunLoopSourceRef = *mut c_void;
    pub type CFMachPortRef = *mut c_void;
    pub type IONotificationPortRef = *mut c_void;

    /// `mach_msg_header_t` (`mach/message.h`).
    #[repr(C)]
    pub struct MachMsgHeader {
        pub bits: u32,
        pub size: u32,
        pub remote_port: u32,
        pub local_port: u32,
        pub voucher_port: u32,
        pub id: i32,
    }

    /// `CFMachPortContext` (`CoreFoundation/CFMachPort.h`).
    #[repr(C)]
    pub struct CFMachPortContext {
        pub version: CFIndex,
        pub info: *mut c_void,
        pub retain: *const c_void,
        pub release: *const c_void,
        pub copy_description: *const c_void,
    }

    pub type CFMachPortCallBack =
        Option<unsafe extern "C" fn(CFMachPortRef, *mut c_void, CFIndex, *mut c_void)>;
    pub type IOServiceInterestCallback =
        Option<unsafe extern "C" fn(*mut c_void, u32, u32, *mut c_void)>;
    pub type IOServiceMatchingCallback = Option<unsafe extern "C" fn(*mut c_void, u32)>;

    /// `NOTIFY_REUSE` (`notify.h`): register on the port passed in.
    pub const NOTIFY_REUSE: c_int = 1;
    /// `kCFStringEncodingUTF8`.
    pub const UTF8: u32 = 0x0800_0100;
    /// `kCFNumberSInt64Type`.
    pub const SINT64: CFIndex = 4;

    unsafe extern "C" {
        pub fn notify_register_mach_port(
            name: *const c_char,
            notify_port: *mut u32,
            flags: c_int,
            out_token: *mut c_int,
        ) -> u32;
        pub fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
        pub fn notify_check(token: c_int, check: *mut c_int) -> u32;
        pub fn notify_get_state(token: c_int, state: *mut u64) -> u32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub static kCFRunLoopDefaultMode: CFStringRef;
        pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
        pub fn CFRunLoopRun();
        pub fn CFMachPortCreateWithPort(
            allocator: CFAllocatorRef,
            port: u32,
            callout: CFMachPortCallBack,
            context: *mut CFMachPortContext,
            should_free_info: *mut u8,
        ) -> CFMachPortRef;
        pub fn CFMachPortCreateRunLoopSource(
            allocator: CFAllocatorRef,
            port: CFMachPortRef,
            order: CFIndex,
        ) -> CFRunLoopSourceRef;
        pub fn CFStringCreateWithCString(
            allocator: CFAllocatorRef,
            cstr: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        pub fn CFStringGetCString(
            s: CFStringRef,
            buf: *mut c_char,
            size: CFIndex,
            encoding: u32,
        ) -> u8;
        pub fn CFRelease(cf: CFTypeRef);
        pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
        pub fn CFBooleanGetTypeID() -> usize;
        pub fn CFBooleanGetValue(b: CFTypeRef) -> u8;
        pub fn CFNumberGetTypeID() -> usize;
        pub fn CFNumberGetValue(n: CFTypeRef, kind: CFIndex, out: *mut c_void) -> u8;
        pub fn CFStringGetTypeID() -> usize;
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub fn IONotificationPortCreate(main_port: u32) -> IONotificationPortRef;
        pub fn IONotificationPortGetRunLoopSource(
            port: IONotificationPortRef,
        ) -> CFRunLoopSourceRef;
        pub fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        pub fn IOServiceGetMatchingService(main_port: u32, matching: *const c_void) -> u32;
        pub fn IOServiceAddInterestNotification(
            port: IONotificationPortRef,
            service: u32,
            interest: *const c_char,
            callback: IOServiceInterestCallback,
            refcon: *mut c_void,
            notification: *mut u32,
        ) -> i32;
        pub fn IOServiceAddMatchingNotification(
            port: IONotificationPortRef,
            kind: *const c_char,
            matching: *const c_void,
            callback: IOServiceMatchingCallback,
            refcon: *mut c_void,
            iterator: *mut u32,
        ) -> i32;
        pub fn IOIteratorNext(iterator: u32) -> u32;
        pub fn IOObjectRelease(object: u32) -> i32;
        pub fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
        pub fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: CFStringRef,
            allocator: CFAllocatorRef,
            options: u32,
        ) -> CFTypeRef;
        pub fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        pub fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFStringRef;
    }
}

/// The notify names A4 registers, each with its mach-port and check tokens once registered.
#[cfg(target_os = "macos")]
const NAMES: [(&str, &std::ffi::CStr); 4] = [
    ("pressure", c"com.apple.system.thermalpressurelevel"),
    ("power_source", c"com.apple.system.powersources.source"),
    ("charge", c"com.apple.system.powersources.percent"),
    ("low_power", c"com.apple.system.lowpowermode"),
];

/// (what, port token, check token).
#[cfg(target_os = "macos")]
static TOKENS: OnceLock<Vec<(&'static str, i32, i32)>> = OnceLock::new();
/// The battery's registry entry (0 where none), for `ExternalConnected` and `UpdateTime`.
#[cfg(target_os = "macos")]
static BATTERY: OnceLock<u32> = OnceLock::new();
/// The IOKit notification port, for the per-framebuffer interest the matching callback adds.
#[cfg(target_os = "macos")]
static IOPORT: OnceLock<usize> = OnceLock::new();
/// Whether each framebuffer's general interest is registered too (`--fb-interest`): once on
/// 2026-11-06 it carried 304 graphics messages (code 0x103) in 5.05 s, cause not identified, so
/// A4's docked reader takes the matching notifications alone.
#[cfg(target_os = "macos")]
static FB_INTEREST: OnceLock<bool> = OnceLock::new();

#[cfg(target_os = "macos")]
fn cfstr(s: &std::ffi::CStr) -> ffi::CFStringRef {
    // SAFETY: a NUL-terminated string; the result is released by the caller.
    unsafe { ffi::CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), ffi::UTF8) }
}

/// A registry property as a JSON scalar: a boolean, an integer, or a string.
#[cfg(target_os = "macos")]
fn prop(entry: u32, key: &std::ffi::CStr) -> String {
    if entry == 0 {
        return "null".to_owned();
    }
    let k = cfstr(key);
    // SAFETY: a live registry entry and a CFString key; the result is +1 and released below.
    let v = unsafe { ffi::IORegistryEntryCreateCFProperty(entry, k, std::ptr::null(), 0) };
    // SAFETY: `k` was created above.
    unsafe { ffi::CFRelease(k) };
    if v.is_null() {
        return "null".to_owned();
    }
    // SAFETY: `v` is a live CF object; each branch reads it as the type its id names.
    let out = unsafe {
        let id = ffi::CFGetTypeID(v);
        if id == ffi::CFBooleanGetTypeID() {
            (ffi::CFBooleanGetValue(v) != 0).to_string()
        } else if id == ffi::CFNumberGetTypeID() {
            let mut n = 0i64;
            if ffi::CFNumberGetValue(v, ffi::SINT64, (&raw mut n).cast()) != 0 {
                n.to_string()
            } else {
                "null".to_owned()
            }
        } else if id == ffi::CFStringGetTypeID() {
            format!("\"{}\"", cfstring(v))
        } else {
            "\"other\"".to_owned()
        }
    };
    // SAFETY: `v` was returned +1 above.
    unsafe { ffi::CFRelease(v) };
    out
}

#[cfg(target_os = "macos")]
fn cfstring(s: ffi::CFStringRef) -> String {
    let mut buf = [0 as std::ffi::c_char; 128];
    // SAFETY: `s` is a live CFString; `buf` holds 128 bytes.
    let ok = unsafe { ffi::CFStringGetCString(s, buf.as_mut_ptr(), 128, ffi::UTF8) } != 0;
    if !ok {
        return String::new();
    }
    // SAFETY: CFStringGetCString wrote a NUL-terminated string into `buf`.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// What the power-source snapshot says provides power now ("AC Power", "Battery Power").
#[cfg(target_os = "macos")]
fn providing() -> String {
    // SAFETY: no arguments; the snapshot is +1 and released below, the type string borrowed.
    unsafe {
        let snap = ffi::IOPSCopyPowerSourcesInfo();
        if snap.is_null() {
            return String::new();
        }
        let t = ffi::IOPSGetProvidingPowerSourceType(snap);
        let s = if t.is_null() {
            String::new()
        } else {
            cfstring(t)
        };
        ffi::CFRelease(snap);
        s
    }
}

/// A notify message on the shared port: its id is the token that fired.
#[cfg(target_os = "macos")]
unsafe extern "C" fn on_notify(
    _port: ffi::CFMachPortRef,
    msg: *mut std::ffi::c_void,
    _size: ffi::CFIndex,
    _info: *mut std::ffi::c_void,
) {
    let (mono, wall) = (mono_ms(), unix_ms());
    // SAFETY: CoreFoundation hands the received message, which starts with its header.
    let id = unsafe { (*msg.cast::<ffi::MachMsgHeader>()).id };
    let Some(&(what, port_token, check_token)) = TOKENS
        .get()
        .and_then(|t| t.iter().find(|(_, p, _)| *p == id))
    else {
        log(&format!(
            "{{\"mono_ms\":{mono:.1},\"unix_ms\":{wall},\"kind\":\"notify\",\"token\":{id},\"what\":\"unknown\"}}"
        ));
        return;
    };
    let read = Instant::now();
    let mut state = 0u64;
    // SAFETY: a token this process registered and a writable state.
    let got = unsafe { ffi::notify_get_state(port_token, &raw mut state) } == 0;
    let mut check = 0i32;
    // SAFETY: a check token this process registered.
    let _ = unsafe { ffi::notify_check(check_token, &raw mut check) };
    let mut extra = String::new();
    if what == "power_source" {
        let battery = BATTERY.get().copied().unwrap_or(0);
        let src = providing();
        let ext = prop(battery, c"ExternalConnected");
        let upd = prop(battery, c"UpdateTime");
        let latency = upd
            .parse::<i64>()
            .map_or_else(|_| "null".to_owned(), |s| (wall - s * 1000).to_string());
        extra = format!(
            ",\"providing\":\"{src}\",\"external_connected\":{ext},\"battery_update_s\":{upd},\"latency_ms_vs_battery_update\":{latency}"
        );
    } else if what == "charge" {
        let battery = BATTERY.get().copied().unwrap_or(0);
        extra = format!(
            ",\"current_capacity\":{},\"external_connected\":{}",
            prop(battery, c"CurrentCapacity"),
            prop(battery, c"ExternalConnected")
        );
    }
    let read_us = read.elapsed().as_secs_f64() * 1e6;
    log(&format!(
        "{{\"mono_ms\":{mono:.1},\"unix_ms\":{wall},\"kind\":\"notify\",\"what\":\"{what}\",\"state\":{},\"check_token_fired\":{}{extra},\"read_us\":{read_us:.0}}}",
        if got {
            state.to_string()
        } else {
            "null".to_owned()
        },
        check != 0
    ));
}

/// A root-domain or framebuffer interest message.
#[cfg(target_os = "macos")]
unsafe extern "C" fn on_interest(
    refcon: *mut std::ffi::c_void,
    service: u32,
    message: u32,
    argument: *mut std::ffi::c_void,
) {
    let (mono, wall) = (mono_ms(), unix_ms());
    let source = if refcon.is_null() {
        "root_domain"
    } else {
        "framebuffer"
    };
    let mut id = 0u64;
    // SAFETY: a live service the callback was registered on; a writable id.
    let _ = unsafe { ffi::IORegistryEntryGetRegistryEntryID(service, &raw mut id) };
    /// `kIOPMMessageClamshellStateChange`: `iokit_family_msg(sub_iokit_powermanagement, 0x100)`.
    const CLAMSHELL: u32 = 0xE003_4100;
    let mut extra = String::new();
    if source == "root_domain" && message == CLAMSHELL {
        let bits = argument as usize;
        let read = Instant::now();
        let closed = aterm_sysprobe::power::lid_closed();
        let read_us = read.elapsed().as_secs_f64() * 1e6;
        let since_power = LAST_POWER.lock().ok().and_then(|g| *g).map_or_else(
            || "null".to_owned(),
            |(ms, what)| format!("[{},\"{what}\"]", wall - ms),
        );
        if let Ok(mut g) = LAST_LID.lock() {
            *g = Some(wall);
        }
        extra = format!(
            ",\"clamshell_closed_bit\":{},\"clamshell_sleep_bit\":{},\"lid_closed_read\":{},\"read_us\":{read_us:.0},\"ms_since_last_power_message\":{since_power}",
            bits & 1 != 0,
            bits & 2 != 0,
            closed.map_or_else(|| "null".to_owned(), |c| c.to_string())
        );
    }
    log(&format!(
        "{{\"mono_ms\":{mono:.1},\"unix_ms\":{wall},\"kind\":\"interest\",\"source\":\"{source}\",\"entry\":{id},\"message\":\"{message:#010x}\",\"argument\":{}{extra}}}",
        argument as usize
    ));
}

/// A framebuffer matched or ended; drains the iterator, which arms it again.
#[cfg(target_os = "macos")]
unsafe extern "C" fn on_match(refcon: *mut std::ffi::c_void, iterator: u32) {
    drain(refcon as usize == 1, iterator, true);
}

#[cfg(target_os = "macos")]
fn drain(terminated: bool, iterator: u32, logged: bool) -> u32 {
    let mut n = 0;
    loop {
        // SAFETY: a live iterator this process holds.
        let s = unsafe { ffi::IOIteratorNext(iterator) };
        if s == 0 {
            break;
        }
        n += 1;
        let mut id = 0u64;
        // SAFETY: a live service; a writable id.
        let _ = unsafe { ffi::IORegistryEntryGetRegistryEntryID(s, &raw mut id) };
        let external = prop(s, c"external");
        if !terminated
            && FB_INTEREST.get().copied().unwrap_or(false)
            && let Some(&port) = IOPORT.get()
        {
            let mut note = 0u32;
            // SAFETY: the notification port this process made; a live service; a non-null
            // refcon marks a framebuffer.
            let _ = unsafe {
                ffi::IOServiceAddInterestNotification(
                    port as ffi::IONotificationPortRef,
                    s,
                    c"IOGeneralInterest".as_ptr(),
                    Some(on_interest),
                    std::ptr::dangling_mut(),
                    &raw mut note,
                )
            };
        }
        if logged {
            log(&format!(
                "{{\"mono_ms\":{:.1},\"unix_ms\":{},\"kind\":\"framebuffer\",\"event\":\"{}\",\"entry\":{id},\"external\":{external}}}",
                mono_ms(),
                unix_ms(),
                if terminated { "terminated" } else { "matched" }
            ));
        }
        if terminated {
            // SAFETY: the iterator handed this reference; a matched one stays held for its
            // interest registration.
            let _ = unsafe { ffi::IOObjectRelease(s) };
        }
    }
    n
}

#[cfg(target_os = "macos")]
fn listen(args: &[String]) -> i32 {
    let _ = T0.set(Instant::now());
    let Some(path) = flag(args, "--out") else {
        eprintln!("listen: --out FILE");
        return 2;
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        eprintln!("listen: cannot write {path}");
        return 1;
    };
    let _ = LOG.set(Mutex::new(file));
    let _ = FB_INTEREST.set(args.iter().any(|a| a == "--fb-interest"));
    let mut regs = Vec::new();

    // The notify names, on one mach port, beside a check token each.
    let mut port = 0u32;
    let mut tokens = Vec::new();
    for (i, (what, name)) in NAMES.iter().enumerate() {
        let mut token = 0i32;
        let flags = if i == 0 { 0 } else { ffi::NOTIFY_REUSE };
        // SAFETY: a NUL-terminated name, a port this call makes (or reuses) and a writable token.
        let rc = unsafe {
            ffi::notify_register_mach_port(name.as_ptr(), &raw mut port, flags, &raw mut token)
        };
        let mut check = 0i32;
        // SAFETY: as above.
        let rc_check = unsafe { ffi::notify_register_check(name.as_ptr(), &raw mut check) };
        regs.push(format!(
            "\"{what}\":{{\"port_rc\":{rc},\"check_rc\":{rc_check}}}"
        ));
        if rc == 0 {
            tokens.push((*what, token, check));
        }
    }
    let _ = TOKENS.set(tokens);
    // SAFETY: CoreFoundation calls on_notify on this thread's run loop with each message; the
    // run-loop source keeps the CFMachPort alive for the process's life.
    unsafe {
        let mut ctx = ffi::CFMachPortContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        };
        let mut free = 0u8;
        let cfport = ffi::CFMachPortCreateWithPort(
            std::ptr::null(),
            port,
            Some(on_notify),
            &raw mut ctx,
            &raw mut free,
        );
        if cfport.is_null() {
            regs.push("\"notify_port\":\"refused\"".to_owned());
        } else {
            let src = ffi::CFMachPortCreateRunLoopSource(std::ptr::null(), cfport, 0);
            ffi::CFRunLoopAddSource(ffi::CFRunLoopGetCurrent(), src, ffi::kCFRunLoopDefaultMode);
        }
    }

    // IOKit: the root domain's interest, the framebuffers' matching and interest.
    // SAFETY: each call takes what the one before it made; the matching dictionaries are
    // consumed by the call that takes them; every object is kept for the process's life.
    unsafe {
        let ioport = ffi::IONotificationPortCreate(0);
        let _ = IOPORT.set(ioport as usize);
        ffi::CFRunLoopAddSource(
            ffi::CFRunLoopGetCurrent(),
            ffi::IONotificationPortGetRunLoopSource(ioport),
            ffi::kCFRunLoopDefaultMode,
        );
        let root =
            ffi::IOServiceGetMatchingService(0, ffi::IOServiceMatching(c"IOPMrootDomain".as_ptr()));
        let mut note = 0u32;
        let rc = if root == 0 {
            -1
        } else {
            ffi::IOServiceAddInterestNotification(
                ioport,
                root,
                c"IOGeneralInterest".as_ptr(),
                Some(on_interest),
                std::ptr::null_mut(),
                &raw mut note,
            )
        };
        regs.push(format!("\"root_domain_interest_rc\":{rc}"));
        let battery = ffi::IOServiceGetMatchingService(
            0,
            ffi::IOServiceMatching(c"AppleSmartBattery".as_ptr()),
        );
        let _ = BATTERY.set(battery);
        for (kind, refcon) in [
            (c"IOServiceFirstMatch", 0usize),
            (c"IOServiceTerminate", 1usize),
        ] {
            let mut iter = 0u32;
            let rc = ffi::IOServiceAddMatchingNotification(
                ioport,
                kind.as_ptr(),
                ffi::IOServiceMatching(c"IOMobileFramebufferShim".as_ptr()),
                Some(on_match),
                refcon as *mut std::ffi::c_void,
                &raw mut iter,
            );
            let n = if rc == 0 {
                drain(refcon == 1, iter, false)
            } else {
                0
            };
            regs.push(format!(
                "\"framebuffer_{}_rc\":{rc},\"framebuffer_{}_now\":{n}",
                if refcon == 0 { "match" } else { "terminate" },
                if refcon == 0 { "match" } else { "terminate" }
            ));
        }
    }

    // Sleep and wake, through aterm's own listener on its thread.
    let listening = aterm_uds::power::listen(Box::new(|p| {
        let wall = unix_ms();
        let what = match p {
            aterm_uds::power::Power::WillSleep => "will_sleep",
            aterm_uds::power::Power::Woke => "woke",
        };
        let since_lid = LAST_LID
            .lock()
            .ok()
            .and_then(|g| *g)
            .map_or_else(|| "null".to_owned(), |ms| (wall - ms).to_string());
        if let Ok(mut g) = LAST_POWER.lock() {
            *g = Some((wall, what));
        }
        log(&format!(
            "{{\"mono_ms\":{:.1},\"unix_ms\":{wall},\"kind\":\"system_power\",\"what\":\"{what}\",\"ms_since_last_clamshell\":{since_lid}}}",
            mono_ms()
        ));
    }));
    regs.push(format!("\"system_power_listener\":{}", listening.is_some()));

    if let Some(ms) = flag(args, "--control-sleep-ms").and_then(|v| v.parse::<u64>().ok()) {
        regs.push(format!("\"control_sleep_ms\":{ms}"));
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(ms.max(1)));
            }
        });
    }

    let pid = std::process::id();
    let lid = aterm_sysprobe::power::lid_closed();
    log(&format!(
        "{{\"mono_ms\":{:.1},\"unix_ms\":{},\"kind\":\"ready\",\"pid\":{pid},\"registrations\":{{{}}},\"lid_closed\":{},\"providing\":\"{}\"}}",
        mono_ms(),
        unix_ms(),
        regs.join(","),
        lid.map_or_else(|| "null".to_owned(), |c| c.to_string()),
        providing()
    ));
    println!("ready {pid}");
    let _ = std::io::stdout().flush();
    // SAFETY: runs this thread's run loop until the process ends; nothing here arms a timer.
    unsafe { ffi::CFRunLoopRun() };
    drop(listening);
    0
}
