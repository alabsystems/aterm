// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wrappers dyld puts in place of the clock calls, their table, and the
//! constructor that reads the lab's variables. Every wrapper counts the read,
//! calls the real function (this image's own call, which no interposing
//! reaches) and shifts the answer when the clock is one a sleep moves
//! ([`crate::model::shifted`]). Nothing panics or allocates on a read; the
//! offset file is looked at under one flag, by one thread at a time.

use core::ffi::{c_int, c_void};
use std::cell::Cell;
use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use crate::model::{
    CLOCK_REALTIME, NCLOCK, Record, Text, ns_to_ticks, parse_offset, render, shift_nsec,
    shift_ticks, shift_timespec, shift_timeval, shifted,
};

/// The variable that names the offset file.
const FILE_VAR: &str = "LABCLOCK_FILE";
/// The variable that names the directory of `<pid>.txt` records.
const DIR_VAR: &str = "LABCLOCK_DIR";
/// The variable whose `1` shifts the clocks that count a sleep too.
const CONTINUOUS_VAR: &str = "LABCLOCK_CONTINUOUS";

/// `struct timespec`.
#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

/// `struct timeval` (`suseconds_t` is 32 bits on Darwin).
#[repr(C)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i32,
}

/// `mach_timebase_info_data_t`.
#[repr(C)]
#[derive(Default)]
struct Timebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn clock_gettime(clock: u32, tp: *mut Timespec) -> c_int;
    safe fn clock_gettime_nsec_np(clock: u32) -> u64;
    fn gettimeofday(tv: *mut Timeval, tz: *mut c_void) -> c_int;
    fn time(out: *mut i64) -> i64;
    safe fn mach_continuous_time() -> u64;
    safe fn mach_continuous_approximate_time() -> u64;
    safe fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut Timebase) -> c_int;
}

/// What the constructor read once: the lab's variables, this executable, and
/// the look intervals in Mach ticks.
struct Config {
    file: Option<PathBuf>,
    dir: Option<PathBuf>,
    continuous: bool,
    exe: String,
    numer: u32,
    denom: u32,
    /// 20 ms: how often the offset file is looked at, at most.
    look: u64,
    /// 2 s: how often the record is written while clocks are read.
    record: u64,
}

/// Set once the constructor has run; until then every read passes unshifted.
static CONFIG: OnceLock<Config> = OnceLock::new();
/// The offset, in nanoseconds.
static OFF_NS: AtomicI64 = AtomicI64::new(0);
/// The uptime tick from which the offset file is looked at again.
static NEXT_LOOK: AtomicU64 = AtomicU64::new(0);
/// The uptime tick from which the record is written again.
static NEXT_RECORD: AtomicU64 = AtomicU64::new(0);
/// Held by the one thread looking at the file.
static LOOKING: AtomicBool = AtomicBool::new(false);
/// The offset file last read: its inode, size and modification time.
static SEEN_INO: AtomicU64 = AtomicU64::new(0);
static SEEN_SIZE: AtomicI64 = AtomicI64::new(-1);
static SEEN_MTIME: AtomicI64 = AtomicI64::new(0);
static SEEN_MTIME_NS: AtomicI64 = AtomicI64::new(0);
/// The reads of the offset file and of each clock call.
static FILE_READS: AtomicU64 = AtomicU64::new(0);
static N_CLOCK_GETTIME: [AtomicU64; NCLOCK] = [const { AtomicU64::new(0) }; NCLOCK];
static N_NSEC_NP: [AtomicU64; NCLOCK] = [const { AtomicU64::new(0) }; NCLOCK];
static N_GETTIMEOFDAY: AtomicU64 = AtomicU64::new(0);
static N_TIME: AtomicU64 = AtomicU64::new(0);
static N_CONTINUOUS: AtomicU64 = AtomicU64::new(0);
static N_CONTINUOUS_APPROX: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Whether this thread is inside a wrapper already.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

/// This thread's way into a wrapper: `None` when it is inside one already (a
/// call the system makes inside a wrapped one, which passes through) or its
/// thread-locals are gone.
struct Outer;

impl Outer {
    fn enter() -> Option<Outer> {
        INSIDE
            .try_with(|inside| (!inside.replace(true)).then_some(Outer))
            .ok()
            .flatten()
    }
}

impl Drop for Outer {
    fn drop(&mut self) {
        let _ = INSIDE.try_with(|inside| inside.set(false));
    }
}

/// Count one read of `clock` in `table`.
fn count(table: &[AtomicU64; NCLOCK], clock: u32) {
    if let Some(n) = usize::try_from(clock).ok().and_then(|i| table.get(i)) {
        n.fetch_add(1, Ordering::Relaxed);
    }
}

/// The configuration and this thread's way in, when a read may be shifted.
fn shifting() -> Option<(&'static Config, Outer)> {
    let config = CONFIG.get()?;
    let outer = Outer::enter()?;
    look(config);
    Some((config, outer))
}

/// Look at the offset file when 20 ms of uptime have passed since the last
/// look, and write the record when the offset changed or 2 s have passed.
fn look(config: &Config) {
    let Some(file) = &config.file else {
        return;
    };
    let now = mach_absolute_time();
    if now < NEXT_LOOK.load(Ordering::Relaxed) || LOOKING.swap(true, Ordering::Acquire) {
        return;
    }
    NEXT_LOOK.store(now.saturating_add(config.look), Ordering::Relaxed);
    let read = reread(file);
    if read || now >= NEXT_RECORD.load(Ordering::Relaxed) {
        NEXT_RECORD.store(now.saturating_add(config.record), Ordering::Relaxed);
        write_record(config);
    }
    LOOKING.store(false, Ordering::Release);
}

/// Read the offset file again when it is not the one last read: whether a
/// new offset was taken.
fn reread(file: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(file) else {
        return false;
    };
    let size = i64::try_from(meta.size()).unwrap_or(i64::MAX);
    if meta.ino() == SEEN_INO.load(Ordering::Relaxed)
        && size == SEEN_SIZE.load(Ordering::Relaxed)
        && meta.mtime() == SEEN_MTIME.load(Ordering::Relaxed)
        && meta.mtime_nsec() == SEEN_MTIME_NS.load(Ordering::Relaxed)
    {
        return false;
    }
    let mut buf = [0u8; 64];
    let Ok(n) = std::fs::File::open(file).and_then(|mut f| f.read(&mut buf)) else {
        return false;
    };
    let Some(off) = buf.get(..n).and_then(parse_offset) else {
        return false;
    };
    OFF_NS.store(off, Ordering::Relaxed);
    SEEN_INO.store(meta.ino(), Ordering::Relaxed);
    SEEN_SIZE.store(size, Ordering::Relaxed);
    SEEN_MTIME.store(meta.mtime(), Ordering::Relaxed);
    SEEN_MTIME_NS.store(meta.mtime_nsec(), Ordering::Relaxed);
    FILE_READS.fetch_add(1, Ordering::Relaxed);
    true
}

/// Write `<dir>/<pid>.txt` by rename, when the lab named a directory.
fn write_record(config: &Config) {
    let Some(dir) = &config.dir else {
        return;
    };
    let load = |n: &AtomicU64| n.load(Ordering::Relaxed);
    let pid = std::process::id();
    let record = Record {
        pid,
        ppid: std::os::unix::process::parent_id(),
        exe: &config.exe,
        offset_ns: OFF_NS.load(Ordering::Relaxed),
        continuous: config.continuous,
        file_reads: load(&FILE_READS),
        gettimeofday: load(&N_GETTIMEOFDAY),
        time: load(&N_TIME),
        continuous_time: load(&N_CONTINUOUS),
        continuous_approximate_time: load(&N_CONTINUOUS_APPROX),
        clock_gettime: N_CLOCK_GETTIME.each_ref().map(load),
        clock_gettime_nsec_np: N_NSEC_NP.each_ref().map(load),
    };
    let mut body: Text<4096> = Text::new();
    let mut path: Text<1200> = Text::new();
    let mut tmp: Text<1200> = Text::new();
    let mut digits: Text<16> = Text::new();
    if render(&record, &mut body).is_err()
        || std::fmt::Write::write_fmt(&mut digits, format_args!("/{pid}.txt")).is_err()
        || path.push(dir.as_os_str().as_bytes()).is_err()
        || path.push(digits.bytes()).is_err()
        || tmp.push(path.bytes()).is_err()
        || tmp.push(b".tmp").is_err()
    {
        return;
    }
    let (path, tmp) = (
        Path::new(OsStr::from_bytes(path.bytes())),
        Path::new(OsStr::from_bytes(tmp.bytes())),
    );
    if std::fs::write(tmp, body.bytes()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

unsafe extern "C" fn lab_clock_gettime(clock: u32, tp: *mut Timespec) -> c_int {
    count(&N_CLOCK_GETTIME, clock);
    let Some((config, _outer)) = shifting() else {
        // SAFETY: the caller's own arguments, handed to the call it made.
        return unsafe { clock_gettime(clock, tp) };
    };
    // SAFETY: as above.
    let r = unsafe { clock_gettime(clock, tp) };
    if r == 0 && shifted(clock, config.continuous) {
        // SAFETY: the call succeeded, so `tp` is the caller's writable timespec (or null).
        if let Some(t) = unsafe { tp.as_mut() } {
            (t.tv_sec, t.tv_nsec) =
                shift_timespec(t.tv_sec, t.tv_nsec, OFF_NS.load(Ordering::Relaxed));
        }
    }
    r
}

extern "C" fn lab_clock_gettime_nsec_np(clock: u32) -> u64 {
    count(&N_NSEC_NP, clock);
    let Some((config, _outer)) = shifting() else {
        return clock_gettime_nsec_np(clock);
    };
    let v = clock_gettime_nsec_np(clock);
    if shifted(clock, config.continuous) {
        shift_nsec(v, OFF_NS.load(Ordering::Relaxed))
    } else {
        v
    }
}

unsafe extern "C" fn lab_gettimeofday(tv: *mut Timeval, tz: *mut c_void) -> c_int {
    N_GETTIMEOFDAY.fetch_add(1, Ordering::Relaxed);
    let Some((_config, _outer)) = shifting() else {
        // SAFETY: the caller's own arguments, handed to the call it made.
        return unsafe { gettimeofday(tv, tz) };
    };
    // SAFETY: as above.
    let r = unsafe { gettimeofday(tv, tz) };
    if r == 0 {
        // SAFETY: the call succeeded, so `tv` is the caller's writable timeval (or null).
        if let Some(t) = unsafe { tv.as_mut() } {
            let (sec, usec) = shift_timeval(
                t.tv_sec,
                i64::from(t.tv_usec),
                OFF_NS.load(Ordering::Relaxed),
            );
            t.tv_sec = sec;
            t.tv_usec = i32::try_from(usec).unwrap_or(0);
        }
    }
    r
}

unsafe extern "C" fn lab_time(out: *mut i64) -> i64 {
    N_TIME.fetch_add(1, Ordering::Relaxed);
    let Some((_config, _outer)) = shifting() else {
        // SAFETY: the caller's own argument, handed to the call it made.
        return unsafe { time(out) };
    };
    let mut now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a timespec of our own; this image's own call, which no interposing reaches.
    if unsafe { clock_gettime(CLOCK_REALTIME, &raw mut now) } != 0 {
        // SAFETY: the caller's own argument.
        return unsafe { time(out) };
    }
    let (sec, _) = shift_timespec(now.tv_sec, now.tv_nsec, OFF_NS.load(Ordering::Relaxed));
    // SAFETY: `out` is the caller's writable time_t, or null.
    if let Some(o) = unsafe { out.as_mut() } {
        *o = sec;
    }
    sec
}

extern "C" fn lab_mach_continuous_time() -> u64 {
    N_CONTINUOUS.fetch_add(1, Ordering::Relaxed);
    let v = mach_continuous_time();
    match shifting() {
        Some((config, _outer)) if config.continuous => shift_ticks(
            v,
            OFF_NS.load(Ordering::Relaxed),
            config.numer,
            config.denom,
        ),
        _ => v,
    }
}

extern "C" fn lab_mach_continuous_approximate_time() -> u64 {
    N_CONTINUOUS_APPROX.fetch_add(1, Ordering::Relaxed);
    let v = mach_continuous_approximate_time();
    match shifting() {
        Some((config, _outer)) if config.continuous => shift_ticks(
            v,
            OFF_NS.load(Ordering::Relaxed),
            config.numer,
            config.denom,
        ),
        _ => v,
    }
}

/// The constructor: read the lab's variables, the timebase and this
/// executable once, take the offset, and write the first record.
extern "C" fn init() {
    let mut tb = Timebase::default();
    // SAFETY: a timebase of our own to fill.
    if unsafe { mach_timebase_info(&raw mut tb) } != 0 || tb.numer == 0 || tb.denom == 0 {
        tb = Timebase { numer: 1, denom: 1 };
    }
    let ticks = |ms: i64| {
        u64::try_from(ns_to_ticks(
            ms.saturating_mul(1_000_000),
            tb.numer,
            tb.denom,
        ))
        .unwrap_or(0)
    };
    let path = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let config = CONFIG.get_or_init(|| Config {
        file: path(FILE_VAR),
        dir: path(DIR_VAR),
        continuous: std::env::var_os(CONTINUOUS_VAR).is_some_and(|v| v == "1"),
        exe: std::env::current_exe()
            .map_or_else(|_| "?".to_string(), |p| p.to_string_lossy().into_owned()),
        numer: tb.numer,
        denom: tb.denom,
        look: ticks(20),
        record: ticks(2000),
    });
    let Some(_outer) = Outer::enter() else {
        return;
    };
    if let Some(file) = &config.file {
        reread(file);
    }
    write_record(config);
}

/// One dyld interposing tuple: the wrapper, and the function it stands in for.
#[repr(C)]
struct Tuple {
    replacement: *const c_void,
    replacee: *const c_void,
}

// SAFETY: two code addresses the linker writes and dyld reads at load; nothing writes them after.
unsafe impl Sync for Tuple {}

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE: [Tuple; 6] = [
    Tuple {
        replacement: lab_clock_gettime as *const c_void,
        replacee: clock_gettime as *const c_void,
    },
    Tuple {
        replacement: lab_clock_gettime_nsec_np as *const c_void,
        replacee: clock_gettime_nsec_np as *const c_void,
    },
    Tuple {
        replacement: lab_gettimeofday as *const c_void,
        replacee: gettimeofday as *const c_void,
    },
    Tuple {
        replacement: lab_time as *const c_void,
        replacee: time as *const c_void,
    },
    Tuple {
        replacement: lab_mach_continuous_time as *const c_void,
        replacee: mach_continuous_time as *const c_void,
    },
    Tuple {
        replacement: lab_mach_continuous_approximate_time as *const c_void,
        replacee: mach_continuous_approximate_time as *const c_void,
    },
];

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static INIT: extern "C" fn() = init;
