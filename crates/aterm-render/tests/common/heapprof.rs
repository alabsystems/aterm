// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// A LIVE-HEAP PROFILER FOR WINDOWS, in-process, no window and no child process.
//
// This is the Windows twin of the `LD_PRELOAD` interposer the Linux memory note
// (`docs/measured/memory-footprint-2026-08-24.md` §2) used: a `#[global_allocator]`
// shim that keeps, per ALLOCATION SITE, the bytes that are still live.
//
// WHAT IT SEES
//   * every `alloc`/`realloc`/`dealloc` that goes through Rust's global allocator,
//     which is every `Vec`/`Box`/`HashMap`/`Arc` in aterm and in every Rust crate
//     it links (fontdue, ttf-parser, wgpu's Rust side, ...);
//   * for allocations >= `MIN_TRACK` bytes, the return-address stack that made
//     them (`RtlCaptureStackBackTrace`, symbolized through dbghelp at report
//     time). SYMBOLIZATION DOES NOT WORK ON THIS BOX and the reason is not
//     understood: `SymInitializeW` returns 1 with `GetLastError` 0, and every
//     `SymFromAddr` then fails, so `report` prints raw addresses. Sites are
//     therefore identified by allocation COUNT and SIZE against a known file
//     table — see `docs/measured/win-heap-2026-08-29.md` §1. Fixing this would
//     make the instrument considerably more useful on a workload whose sites are
//     not already known by arithmetic.
//   * exact live/peak byte totals for ALL sizes (the sub-threshold ones are
//     counted, just not attributed).
//
// WHAT IT CANNOT SEE
//   * anything that does not go through Rust's global allocator: direct
//     `HeapAlloc`/`VirtualAlloc`/`malloc` from a C or C++ library (the D3D12
//     runtime, the AMD driver, DirectWrite), thread stacks, loaded module images,
//     and any file mapping. That is the whole GPU stack and most of the OS's own
//     footprint. It is exactly the boundary this task wants: "application heap".
//   * the difference between live bytes and working set. Both are reported so the
//     allocator's own retained-but-free arena is visible as the gap (the Linux
//     note's §14 finding was that gap, not the live bytes).
//
// One report per process: the working set never shrinks back on Windows either,
// so a second measurement in the same process measures the first one's freed
// arena and lies.

#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// Allocations at least this large get a stack. Below it they are only counted.
/// 512 B keeps the live map small and the capture cheap while still catching
/// fontdue's per-glyph geometry (measured ~3.4 kB each on Linux) and every font
/// file buffer. Everything under it is counted in the totals but not attributed,
/// and `report` prints both so the unattributed remainder is always visible —
/// measured at 1,917 kB of 80,006 kB (2.4%) on the full font-stack workload.
pub const MIN_TRACK: usize = 512;
/// Return addresses captured per site.
pub const FRAMES: usize = 16;

// ---------------------------------------------------------------------------
// Win32
// ---------------------------------------------------------------------------

#[link(name = "kernel32")]
unsafe extern "system" {
    fn RtlCaptureStackBackTrace(
        frames_to_skip: u32,
        frames_to_capture: u32,
        back_trace: *mut *mut c_void,
        back_trace_hash: *mut u32,
    ) -> u16;
    fn GetCurrentProcess() -> *mut c_void;
    fn GetLastError() -> u32;
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn K32GetProcessMemoryInfo(
        process: *mut c_void,
        counters: *mut ProcessMemoryCounters,
        cb: u32,
    ) -> i32;
}

/// Process working set in kB — the same counter the GPU memory-floor work used.
#[must_use]
pub fn working_set_kb() -> u64 {
    let mut c = ProcessMemoryCounters {
        cb: u32::try_from(size_of::<ProcessMemoryCounters>()).unwrap_or(0),
        ..ProcessMemoryCounters::default()
    };
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &raw mut c,
            u32::try_from(size_of::<ProcessMemoryCounters>()).unwrap_or(0),
        )
    };
    if ok == 0 {
        return 0;
    }
    (c.working_set_size / 1024) as u64
}

/// Private commit (pagefile usage) in kB — immune to trimming, unlike the
/// working set.
#[must_use]
pub fn private_bytes_kb() -> u64 {
    let mut c = ProcessMemoryCounters {
        cb: u32::try_from(size_of::<ProcessMemoryCounters>()).unwrap_or(0),
        ..ProcessMemoryCounters::default()
    };
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &raw mut c,
            u32::try_from(size_of::<ProcessMemoryCounters>()).unwrap_or(0),
        )
    };
    if ok == 0 {
        return 0;
    }
    (c.pagefile_usage / 1024) as u64
}

// ---------------------------------------------------------------------------
// The allocator shim
// ---------------------------------------------------------------------------

static LIVE_BYTES: AtomicI64 = AtomicI64::new(0);
static PEAK_BYTES: AtomicI64 = AtomicI64::new(0);
static LIVE_COUNT: AtomicI64 = AtomicI64::new(0);
static TOTAL_ALLOCS: AtomicU64 = AtomicU64::new(0);
static TRACKING: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Reentrancy guard: the bookkeeping below allocates, and those allocations
    /// must not be booked.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

#[derive(Default)]
struct Site {
    frames: [usize; FRAMES],
    depth: usize,
    live: i64,
    live_count: i64,
    total: u64,
}

#[derive(Default)]
struct State {
    /// pointer -> (size, site index)
    live: HashMap<usize, (usize, u32)>,
    /// stack hash -> site index
    by_stack: HashMap<u64, u32>,
    sites: Vec<Site>,
}

fn state() -> &'static Mutex<State> {
    static S: std::sync::OnceLock<Mutex<State>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub struct Prof;

impl Prof {
    fn book_alloc(ptr: *mut u8, size: usize) {
        if ptr.is_null() {
            return;
        }
        LIVE_BYTES.fetch_add(size as i64, Ordering::Relaxed);
        LIVE_COUNT.fetch_add(1, Ordering::Relaxed);
        TOTAL_ALLOCS.fetch_add(1, Ordering::Relaxed);
        let live = LIVE_BYTES.load(Ordering::Relaxed);
        PEAK_BYTES.fetch_max(live, Ordering::Relaxed);

        if size < MIN_TRACK || !TRACKING.load(Ordering::Relaxed) {
            return;
        }
        let already = BUSY.with(|b| b.replace(true));
        if already {
            return;
        }
        let mut frames = [std::ptr::null_mut::<c_void>(); FRAMES];
        let mut hash: u32 = 0;
        // Skip this frame and the `GlobalAlloc::alloc` wrapper.
        let n = unsafe {
            RtlCaptureStackBackTrace(
                2,
                u32::try_from(FRAMES).unwrap_or(8),
                frames.as_mut_ptr(),
                &raw mut hash,
            )
        } as usize;
        let mut key: u64 = 0xcbf2_9ce4_8422_2325;
        for f in frames.iter().take(n) {
            key ^= *f as u64;
            key = key.wrapping_mul(0x1000_0000_01b3);
        }
        {
            let mut st = lock();
            let idx = match st.by_stack.get(&key) {
                Some(i) => *i,
                None => {
                    let mut site = Site {
                        depth: n,
                        ..Site::default()
                    };
                    for (dst, src) in site.frames.iter_mut().zip(frames.iter()) {
                        *dst = *src as usize;
                    }
                    st.sites.push(site);
                    let i = u32::try_from(st.sites.len() - 1).unwrap_or(u32::MAX);
                    st.by_stack.insert(key, i);
                    i
                }
            };
            if let Some(site) = st.sites.get_mut(idx as usize) {
                site.live += size as i64;
                site.live_count += 1;
                site.total += size as u64;
            }
            st.live.insert(ptr as usize, (size, idx));
        }
        BUSY.with(|b| b.set(false));
    }

    fn book_dealloc(ptr: *mut u8, size: usize) {
        if ptr.is_null() {
            return;
        }
        LIVE_BYTES.fetch_sub(size as i64, Ordering::Relaxed);
        LIVE_COUNT.fetch_sub(1, Ordering::Relaxed);
        if size < MIN_TRACK {
            return;
        }
        let already = BUSY.with(|b| b.replace(true));
        if already {
            return;
        }
        {
            let mut st = lock();
            if let Some((sz, idx)) = st.live.remove(&(ptr as usize))
                && let Some(site) = st.sites.get_mut(idx as usize)
            {
                site.live -= sz as i64;
                site.live_count -= 1;
            }
        }
        BUSY.with(|b| b.set(false));
    }
}

unsafe impl GlobalAlloc for Prof {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        Self::book_alloc(p, layout.size());
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::book_dealloc(ptr, layout.size());
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        Self::book_alloc(p, layout.size());
        p
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::book_dealloc(ptr, layout.size());
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        Self::book_alloc(p, new_size);
        p
    }
}

/// Begin attributing allocations to stacks. Counting is always on; this only
/// gates the (much more expensive) stack capture.
pub fn start() {
    TRACKING.store(true, Ordering::SeqCst);
}

pub fn stop() {
    TRACKING.store(false, Ordering::SeqCst);
}

#[must_use]
pub fn live_kb() -> i64 {
    LIVE_BYTES.load(Ordering::Relaxed) / 1024
}

#[must_use]
pub fn peak_kb() -> i64 {
    PEAK_BYTES.load(Ordering::Relaxed) / 1024
}

#[must_use]
pub fn live_allocations() -> i64 {
    LIVE_COUNT.load(Ordering::Relaxed)
}

/// A named checkpoint: live heap and working set right now.
pub fn mark(label: &str) {
    eprintln!(
        "[mark] {label:<44} live={:>9} kB  ws={:>9} kB  priv={:>9} kB  allocs={}",
        live_kb(),
        working_set_kb(),
        private_bytes_kb(),
        live_allocations()
    );
}

// ---------------------------------------------------------------------------
// Symbolization (dbghelp)
// ---------------------------------------------------------------------------

#[link(name = "dbghelp")]
unsafe extern "system" {
    fn SymSetOptions(options: u32) -> u32;
    fn SymInitializeW(process: *mut c_void, search_path: *const u16, invade: i32) -> i32;
    fn SymFromAddr(
        process: *mut c_void,
        addr: u64,
        displacement: *mut u64,
        symbol: *mut SymbolInfo,
    ) -> i32;
    fn SymGetLineFromAddr64(
        process: *mut c_void,
        addr: u64,
        displacement: *mut u32,
        line: *mut ImagehlpLine64,
    ) -> i32;
}

const SYMOPT_UNDNAME: u32 = 0x0000_0002;
const SYMOPT_DEFERRED_LOADS: u32 = 0x0000_0004;
const SYMOPT_LOAD_LINES: u32 = 0x0000_0010;
const MAX_SYM_NAME: usize = 1024;

#[repr(C)]
struct SymbolInfo {
    size_of_struct: u32,
    type_index: u32,
    reserved: [u64; 2],
    index: u32,
    size: u32,
    mod_base: u64,
    flags: u32,
    value: u64,
    address: u64,
    register: u32,
    scope: u32,
    tag: u32,
    name_len: u32,
    max_name_len: u32,
    name: [u8; MAX_SYM_NAME],
}

#[repr(C)]
struct ImagehlpLine64 {
    size_of_struct: u32,
    key: *mut c_void,
    line_number: u32,
    file_name: *mut u8,
    address: u64,
}

fn c_str(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut out = Vec::new();
    let mut i = 0_isize;
    loop {
        let b = unsafe { *p.offset(i) };
        if b == 0 || i > 4096 {
            break;
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn sym_init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        SymSetOptions(SYMOPT_UNDNAME | SYMOPT_LOAD_LINES);
        let ok = SymInitializeW(GetCurrentProcess(), std::ptr::null(), 1);
        eprintln!(
            "[heapprof] SymInitializeW -> {ok} (last error {})",
            GetLastError()
        );
    });
}

fn resolve(addr: usize) -> String {
    sym_init();
    if addr == 0 {
        return String::from("<null>");
    }
    let mut info: Box<SymbolInfo> = Box::new(unsafe { std::mem::zeroed() });
    info.size_of_struct = 88; // sizeof(SYMBOL_INFO) without the name tail
    info.max_name_len = u32::try_from(MAX_SYM_NAME - 1).unwrap_or(0);
    let mut disp: u64 = 0;
    let name = if unsafe {
        SymFromAddr(
            GetCurrentProcess(),
            addr as u64,
            &raw mut disp,
            std::ptr::from_mut(&mut *info),
        )
    } != 0
    {
        c_str(info.name.as_ptr())
    } else {
        format!("0x{addr:016x}")
    };
    let mut line: ImagehlpLine64 = unsafe { std::mem::zeroed() };
    line.size_of_struct = u32::try_from(size_of::<ImagehlpLine64>()).unwrap_or(0);
    let mut ldisp: u32 = 0;
    let where_ = if unsafe {
        SymGetLineFromAddr64(
            GetCurrentProcess(),
            addr as u64,
            &raw mut ldisp,
            &raw mut line,
        )
    } != 0
    {
        let f = c_str(line.file_name);
        let short = f.rsplit(['\\', '/']).next().unwrap_or("").to_string();
        format!(" ({}:{})", short, line.line_number)
    } else {
        String::new()
    };
    format!("{name}{where_}")
}

/// Print the top `top` allocation sites by LIVE bytes, deepest frames first.
pub fn report(title: &str, top: usize) {
    stop();
    // The bookkeeping lock is NOT reentrant, and everything below allocates:
    // growing `rows` inside the locked block reallocs, which re-enters
    // `book_dealloc`, which takes the same lock and deadlocks. `BUSY` is the
    // same guard the alloc path uses; hold it for the whole report.
    let outer = BUSY.with(|b| b.replace(true));
    let (mut rows, total_live, total_count) = {
        let st = lock();
        let mut rows: Vec<(i64, i64, [usize; FRAMES], usize)> = st
            .sites
            .iter()
            .filter(|s| s.live > 0)
            .map(|s| (s.live, s.live_count, s.frames, s.depth))
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.0));
        let live: i64 = st.sites.iter().map(|s| s.live).sum();
        (rows, live, st.live.len())
    };
    rows.truncate(top);

    eprintln!("\n================ HEAP ATTRIBUTION: {title} ================");
    eprintln!(
        "live (all sizes)      : {} kB in {} allocations",
        live_kb(),
        live_allocations()
    );
    eprintln!(
        "live (attributed >={MIN_TRACK}B): {} kB in {total_count} allocations",
        total_live / 1024
    );
    eprintln!("peak live             : {} kB", peak_kb());
    eprintln!("working set           : {} kB", working_set_kb());
    eprintln!("private (commit)      : {} kB", private_bytes_kb());
    eprintln!("---------------------------------------------------------------");
    for (i, (live, count, frames, depth)) in rows.iter().enumerate() {
        eprintln!(
            "\n#{:<2} {:>9} kB  {:>7} allocs  ({:.1} kB avg)",
            i + 1,
            live / 1024,
            count,
            if *count > 0 {
                *live as f64 / *count as f64 / 1024.0
            } else {
                0.0
            }
        );
        for f in frames.iter().take(*depth) {
            let s = resolve(*f);
            // Trim the noise frames that every stack shares.
            if s.starts_with("RtlUserThreadStart")
                || s.starts_with("BaseThreadInitThunk")
                || s.contains("__rust_begin_short_backtrace")
            {
                break;
            }
            eprintln!("      {s}");
        }
    }
    eprintln!("=============== END HEAP ATTRIBUTION ===============\n");
    BUSY.with(|b| b.set(outer));
}

/// One priced line item: run `f`, report the live heap it LEAVES BEHIND.
///
/// This is the per-item twin of [`report`]: where the stack attribution says
/// which call site allocated, this says what one named step costs, which is the
/// number a fix is measured against. Both are needed — a step can be cheap and
/// still hold a duplicate, and a site can be expensive and still be earned.
/// [`bill`] without the printing: `(value, live delta kB, own-transient peak kB)`.
pub fn measure<T>(f: impl FnOnce() -> T) -> (T, i64, i64) {
    let before = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(before, Ordering::Relaxed);
    let out = f();
    let after = LIVE_BYTES.load(Ordering::Relaxed);
    let peak = PEAK_BYTES.load(Ordering::Relaxed);
    (out, (after - before) / 1024, (peak - before) / 1024)
}

pub fn bill<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let before = LIVE_BYTES.load(Ordering::Relaxed);
    let ws_before = working_set_kb();
    // Re-arm the high-water mark at the CURRENT level, so the third column is
    // this step's own transient rather than the whole process's history.
    PEAK_BYTES.store(before, Ordering::Relaxed);
    let t0 = std::time::Instant::now();
    let out = f();
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let after = LIVE_BYTES.load(Ordering::Relaxed);
    let peak = PEAK_BYTES.load(Ordering::Relaxed);
    eprintln!(
        "[bill] {label:<52} live +{:>8} kB   peak +{:>8} kB   ws +{:>8} kB   {ms:>8.1} ms",
        (after - before) / 1024,
        (peak - before) / 1024,
        working_set_kb() as i64 - ws_before as i64,
    );
    out
}
