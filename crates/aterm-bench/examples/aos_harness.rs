// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos` performance lane harness (`verify perf aos`) measuring momentum glow under Aos.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use aterm_aos::checkpoint::{Checkpoint, Provenance};
use aterm_aos::contract::Verdict;
use aterm_aos::effects::{NoEffects, ShadowPort};
use aterm_aos::log::{EventLog, LogHeader, Tail};
use aterm_aos::replay::{chain_link, empty_chain, replay};
use aterm_aos::task::{Code, PausedClass, Schema, state_digest};
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{ArtifactId, ArtifactKind, TaskIdentity};
use aterm_effects::aos_momentum::{
    ConfigRecord, EventCounts, GeomRecord, MOMENTUM_MAX_EVENTS, MomentumCode, MomentumEvent,
    MomentumHost, MomentumSchema, MomentumState, MomentumTrace, PrebuiltMomentum,
    momentum_identity,
};
use aterm_effects::cursor_glow::Geom;
use aterm_effects::cursor_momentum::{
    MOMENTUM_GLOW_RADIUS_CELLS, MOMENTUM_GLOW_TAU_S, MomentumGlow, MomentumGlowConfig,
    MomentumGlowFrame,
};
use aterm_render::GlowQuad;

/// `System`, counted: every allocation, deallocation and reallocation, the bytes asked for,
/// the bytes live and their peak.
struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static DEALLOCS: AtomicU64 = AtomicU64::new(0);
static REALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);

fn grew(by: u64) {
    let live = LIVE.fetch_add(by, Relaxed).wrapping_add(by);
    PEAK.fetch_max(live, Relaxed);
}

// SAFETY: forwards arguments to `System` and updates atomic counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `GlobalAlloc::alloc` contract, passed through.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            ALLOC_BYTES.fetch_add(layout.size() as u64, Relaxed);
            grew(layout.size() as u64);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's `GlobalAlloc::alloc_zeroed` contract, passed through.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            ALLOC_BYTES.fetch_add(layout.size() as u64, Relaxed);
            grew(layout.size() as u64);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's `GlobalAlloc::dealloc` contract, passed through.
        unsafe { System.dealloc(ptr, layout) };
        DEALLOCS.fetch_add(1, Relaxed);
        LIVE.fetch_sub(layout.size() as u64, Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's `GlobalAlloc::realloc` contract, passed through.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            REALLOCS.fetch_add(1, Relaxed);
            ALLOC_BYTES.fetch_add(new_size as u64, Relaxed);
            let (old, new) = (layout.size() as u64, new_size as u64);
            if new >= old {
                grew(new - old);
            } else {
                LIVE.fetch_sub(old - new, Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn live_bytes() -> u64 {
    LIVE.load(Relaxed)
}

const COUNT_NAMES: [&str; 14] = [
    "cpu_ns",
    "instructions",
    "cycles",
    "faults",
    "pageins",
    "cow_faults",
    "syscalls_unix",
    "syscalls_mach",
    "mach_messages",
    "csw",
    "allocs",
    "deallocs",
    "reallocs",
    "alloc_bytes",
];
const CPU_NS: usize = 0;
const INSTRUCTIONS: usize = 1;
const CYCLES: usize = 2;
const FAULTS: usize = 3;
const PAGEINS: usize = 4;
const COW_FAULTS: usize = 5;
const SYSCALLS_UNIX: usize = 6;
const SYSCALLS_MACH: usize = 7;
const MACH_MESSAGES: usize = 8;
const CSW: usize = 9;
const ALLOC_COUNT: usize = 10;
const DEALLOC_COUNT: usize = 11;
const REALLOC_COUNT: usize = 12;
const ALLOC_BYTE_COUNT: usize = 13;

/// Mach's `task_events_info` counters are 32-bit (`integer_t`): their differences wrap there.
fn wraps_at_32(i: usize) -> bool {
    (FAULTS..=CSW).contains(&i)
}

/// What the counters read at one instant (absolute), or what an interval moved (a difference).
#[derive(Clone, Copy, Default)]
struct Count([u64; 14]);

impl Count {
    fn now() -> Count {
        let mut c = [0u64; 14];
        c[ALLOC_COUNT] = ALLOCS.load(Relaxed);
        c[DEALLOC_COUNT] = DEALLOCS.load(Relaxed);
        c[REALLOC_COUNT] = REALLOCS.load(Relaxed);
        c[ALLOC_BYTE_COUNT] = ALLOC_BYTES.load(Relaxed);
        if let Some(r) = os::rusage() {
            c[INSTRUCTIONS] = r.instructions;
            c[CYCLES] = r.cycles;
        }
        if let Some(t) = os::task_events() {
            c[FAULTS] = t.faults;
            c[PAGEINS] = t.pageins;
            c[COW_FAULTS] = t.cow_faults;
            c[SYSCALLS_UNIX] = t.syscalls_unix;
            c[SYSCALLS_MACH] = t.syscalls_mach;
            c[MACH_MESSAGES] = t.messages;
            c[CSW] = t.csw;
        }
        c[CPU_NS] = os::thread_cpu_ns();
        Count(c)
    }

    fn since(self, earlier: Count) -> Count {
        let mut d = [0u64; 14];
        for (i, slot) in d.iter_mut().enumerate() {
            *slot = if wraps_at_32(i) {
                u64::from((self.0[i] as u32).wrapping_sub(earlier.0[i] as u32))
            } else {
                self.0[i].wrapping_sub(earlier.0[i])
            };
        }
        Count(d)
    }

    fn add(&mut self, d: Count) {
        for (a, b) in self.0.iter_mut().zip(d.0) {
            *a = a.saturating_add(b);
        }
    }

    /// `self` less `intervals` empty intervals' worth of the counters' own reads.
    fn net(self, empty: Count, intervals: u64) -> Count {
        let mut n = self;
        for (a, e) in n.0.iter_mut().zip(empty.0) {
            *a = a.saturating_sub(e.saturating_mul(intervals));
        }
        n
    }

    fn median(samples: &[Count]) -> Count {
        let mut m = [0u64; 14];
        for (i, slot) in m.iter_mut().enumerate() {
            let mut v: Vec<u64> = samples.iter().map(|c| c.0[i]).collect();
            v.sort_unstable();
            *slot = v.get(v.len() / 2).copied().unwrap_or(0);
        }
        Count(m)
    }

    fn json(self) -> Obj {
        COUNT_NAMES
            .iter()
            .zip(self.0)
            .fold(Obj::new(), |o, (k, v)| o.u(k, v))
    }

    fn per(self, events: u64) -> Obj {
        COUNT_NAMES
            .iter()
            .zip(self.0)
            .fold(Obj::new(), |o, (k, v)| o.f(k, ratio(v, events)))
    }
}

/// The counters around `f`, and its wall time: counters, clock, `f`, clock, counters.
fn measured<R>(f: impl FnOnce() -> R) -> (R, u64, Count) {
    let before = Count::now();
    let start = Instant::now();
    let r = f();
    let wall = nanos(start.elapsed());
    let after = Count::now();
    (r, wall, after.since(before))
}

/// `f`'s wall time alone, between two clock reads.
#[inline(always)]
fn timed<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let start = Instant::now();
    let r = f();
    let end = Instant::now();
    (r, nanos(end.duration_since(start)))
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

fn ratio(num: u64, den: u64) -> f64 {
    if den == 0 {
        f64::NAN
    } else {
        num as f64 / den as f64
    }
}

/// The platform's counters: macOS reads them, elsewhere they are absent and read as 0.
#[cfg(target_os = "macos")]
mod os {
    pub(super) struct Rusage {
        pub(super) instructions: u64,
        pub(super) cycles: u64,
        pub(super) footprint: u64,
        pub(super) lifetime_max_footprint: u64,
    }

    pub(super) struct TaskEvents {
        pub(super) faults: u64,
        pub(super) pageins: u64,
        pub(super) cow_faults: u64,
        pub(super) syscalls_unix: u64,
        pub(super) syscalls_mach: u64,
        pub(super) messages: u64,
        pub(super) csw: u64,
    }

    /// `task_events_info` (`<mach/task_info.h>`): eight `integer_t`s.
    #[repr(C)]
    #[derive(Default)]
    struct TaskEventsInfo {
        faults: i32,
        pageins: i32,
        cow_faults: i32,
        messages_sent: i32,
        messages_received: i32,
        syscalls_mach: i32,
        syscalls_unix: i32,
        csw: i32,
    }

    const TASK_EVENTS_INFO: u32 = 2;
    const TASK_EVENTS_INFO_COUNT: u32 = 8;

    unsafe extern "C" {
        /// The task's own port; `mach_task_self()` is a macro that reads it.
        #[allow(non_upper_case_globals, reason = "the C symbol's own name")]
        static mach_task_self_: u32;
        fn task_info(
            target_task: u32,
            flavor: u32,
            task_info_out: *mut i32,
            task_info_out_cnt: *mut u32,
        ) -> i32;
        fn getloadavg(loadavg: *mut f64, nelem: i32) -> i32;
    }

    pub(super) fn rusage() -> Option<Rusage> {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        let pid = libc::c_int::try_from(std::process::id()).ok()?;
        // SAFETY: `info` is writable for one `rusage_info_v4`.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        if rc != 0 {
            return None;
        }
        // SAFETY: zeroed at creation, so initialized whatever the call did.
        let info = unsafe { info.assume_init() };
        Some(Rusage {
            instructions: info.ri_instructions,
            cycles: info.ri_cycles,
            footprint: info.ri_phys_footprint,
            lifetime_max_footprint: info.ri_lifetime_max_phys_footprint,
        })
    }

    pub(super) fn task_events() -> Option<TaskEvents> {
        let mut info = TaskEventsInfo::default();
        let mut count = TASK_EVENTS_INFO_COUNT;
        // SAFETY: `info` is eight writable `integer_t`s and `mach_task_self_` is this task's port.
        let kr = unsafe {
            task_info(
                mach_task_self_,
                TASK_EVENTS_INFO,
                (&raw mut info).cast::<i32>(),
                &raw mut count,
            )
        };
        if kr != 0 || count != TASK_EVENTS_INFO_COUNT {
            return None;
        }
        let w = |v: i32| u64::from(v as u32);
        Some(TaskEvents {
            faults: w(info.faults),
            pageins: w(info.pageins),
            cow_faults: w(info.cow_faults),
            syscalls_unix: w(info.syscalls_unix),
            syscalls_mach: w(info.syscalls_mach),
            messages: w(info.messages_sent).wrapping_add(w(info.messages_received)),
            csw: w(info.csw),
        })
    }

    pub(super) fn thread_cpu_ns() -> u64 {
        let mut stamp = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `stamp` is initialized and writable for this one call.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut stamp) } != 0 {
            return 0;
        }
        u64::try_from(stamp.tv_sec).unwrap_or(0) * 1_000_000_000
            + u64::try_from(stamp.tv_nsec).unwrap_or(0)
    }

    pub(super) fn load() -> Option<[f64; 3]> {
        let mut avg = [0.0f64; 3];
        // SAFETY: three writable doubles, and `nelem` says three.
        let n = unsafe { getloadavg(avg.as_mut_ptr(), 3) };
        if n == 3 { Some(avg) } else { None }
    }

    pub(super) fn timebase() -> Option<(u32, u32)> {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `tb` is writable for one `mach_timebase_info`.
        if unsafe { libc::mach_timebase_info(&mut tb) } != 0 {
            return None;
        }
        Some((tb.numer, tb.denom))
    }

    fn sysctl_raw(name: &str) -> Option<Vec<u8>> {
        let cname = std::ffi::CString::new(name).ok()?;
        let mut len: libc::size_t = 0;
        // SAFETY: a size query: no buffer, `len` writable, nothing set.
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` is `len` writable bytes and `len` says so; nothing is set.
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(len);
        Some(buf)
    }

    pub(super) fn sysctl_str(name: &str) -> Option<String> {
        let mut buf = sysctl_raw(name)?;
        while buf.last() == Some(&0) {
            buf.pop();
        }
        String::from_utf8(buf).ok()
    }

    pub(super) fn sysctl_u64(name: &str) -> Option<u64> {
        let buf = sysctl_raw(name)?;
        match buf.len() {
            4 => Some(u64::from(u32::from_ne_bytes(buf.try_into().ok()?))),
            8 => Some(u64::from_ne_bytes(buf.try_into().ok()?)),
            _ => None,
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod os {
    pub(super) struct Rusage {
        pub(super) instructions: u64,
        pub(super) cycles: u64,
        pub(super) footprint: u64,
        pub(super) lifetime_max_footprint: u64,
    }

    pub(super) struct TaskEvents {
        pub(super) faults: u64,
        pub(super) pageins: u64,
        pub(super) cow_faults: u64,
        pub(super) syscalls_unix: u64,
        pub(super) syscalls_mach: u64,
        pub(super) messages: u64,
        pub(super) csw: u64,
    }

    pub(super) fn rusage() -> Option<Rusage> {
        None
    }

    pub(super) fn task_events() -> Option<TaskEvents> {
        None
    }

    pub(super) fn thread_cpu_ns() -> u64 {
        0
    }

    pub(super) fn load() -> Option<[f64; 3]> {
        None
    }

    pub(super) fn timebase() -> Option<(u32, u32)> {
        None
    }

    pub(super) fn sysctl_str(_: &str) -> Option<String> {
        None
    }

    pub(super) fn sysctl_u64(_: &str) -> Option<u64> {
        None
    }
}

/// One JSON object, built in order.
struct Obj(String);

impl Obj {
    fn new() -> Obj {
        Obj(String::new())
    }

    fn key(&mut self, k: &str) {
        if !self.0.is_empty() {
            self.0.push(',');
        }
        quote(&mut self.0, k);
        self.0.push(':');
    }

    fn u(mut self, k: &str, v: u64) -> Obj {
        self.key(k);
        let _ = write!(self.0, "{v}");
        self
    }

    fn opt_u(mut self, k: &str, v: Option<u64>) -> Obj {
        match v {
            Some(v) => self.u(k, v),
            None => {
                self.key(k);
                self.0.push_str("null");
                self
            }
        }
    }

    fn f(mut self, k: &str, v: f64) -> Obj {
        self.key(k);
        if v.is_finite() {
            let _ = write!(self.0, "{v:.3}");
        } else {
            self.0.push_str("null");
        }
        self
    }

    fn t(mut self, k: &str, v: &str) -> Obj {
        self.key(k);
        quote(&mut self.0, v);
        self
    }

    fn b(mut self, k: &str, v: bool) -> Obj {
        self.key(k);
        self.0.push_str(if v { "true" } else { "false" });
        self
    }

    fn o(mut self, k: &str, v: Obj) -> Obj {
        self.key(k);
        self.0.push_str(&v.done());
        self
    }

    fn a(mut self, k: &str, items: Vec<Obj>) -> Obj {
        self.key(k);
        self.0.push('[');
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                self.0.push(',');
            }
            self.0.push_str(&item.done());
        }
        self.0.push(']');
        self
    }

    fn done(self) -> String {
        format!("{{{}}}", self.0)
    }
}

fn quote(s: &mut String, v: &str) {
    s.push('"');
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(s, "\\u{:04x}", u32::from(c));
            }
            c => s.push(c),
        }
    }
    s.push('"');
}

/// A distribution: count, min, nearest-rank p50/p95/p99, max and mean.
fn dist(v: &mut [u64]) -> Obj {
    v.sort_unstable();
    let n = v.len();
    let rank = |pct: usize| -> u64 {
        if n == 0 {
            return 0;
        }
        let r = (n * pct).div_ceil(100).max(1);
        v[r.min(n) - 1]
    };
    let sum: u64 = v.iter().fold(0u64, |a, b| a.saturating_add(*b));
    Obj::new()
        .u("n", n as u64)
        .u("min", v.first().copied().unwrap_or(0))
        .u("p50", rank(50))
        .u("p95", rank(95))
        .u("p99", rank(99))
        .u("max", v.last().copied().unwrap_or(0))
        .f("mean", ratio(sum, n as u64))
}

/// The frame interval the host's lane runs at (the momentum fixtures' `LANE`).
const LANE_NS: u64 = 16_666_667;
const LANE: Duration = Duration::from_nanos(LANE_NS);
const TAU: f32 = MOMENTUM_GLOW_TAU_S;
/// The build id the attached paths name.
const BUILD: &str = "aos-harness";
const ROWS: u16 = 24;
const COLS: u16 = 100;
const KEYS_PER_BURST: u16 = 24;
/// A full cool-down runs until the lane parks, then the desk is idle this long.
const IDLE_AFTER_PARK_NS: u64 = 500_000_000;
/// A cut cool-down ends this long after the burst's last key, still hot.
const CUT_AFTER_NS: u64 = 400_000_000;
/// A hang detector, not a budget: no segment of the story runs a minute of story time.
const HANG_NS: u64 = 60_000_000_000;
/// The capacity the host's quad vectors start with (other effects' share included).
const QUADS: usize = 512;
const RESET_TAG: u64 = 0x5245_5345_5400_0000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Dark,
    Light,
}

#[derive(Clone, Copy)]
enum Cool {
    /// Until the lane parks, then idle.
    Park,
    /// Cut by the next burst 400 ms after the last key.
    Cut,
}

/// (page, keys per second, cool-down) for each burst of 24 keys, in order.
const SEGMENTS: [(Page, u64, Cool); 8] = [
    (Page::Dark, 3, Cool::Park),
    (Page::Dark, 6, Cool::Cut),
    (Page::Dark, 10, Cool::Park),
    (Page::Dark, 16, Cool::Cut),
    (Page::Light, 3, Cool::Park),
    (Page::Light, 6, Cool::Cut),
    (Page::Light, 10, Cool::Park),
    (Page::Light, 16, Cool::Park),
];

fn geom() -> Geom {
    Geom {
        cw: 15,
        ch: 28,
        rows: usize::from(ROWS),
        cols: usize::from(COLS),
        origin_x: 24,
        origin_y: 84,
        win_w: 24 + COLS * 15 + 24,
        win_h: 84 + ROWS * 28 + 24,
        head: 80,
    }
}

/// The host's config for a block caret at column `col` on `page`: the letters left of it
/// are the row's glyph span.
fn config(page: Page, col: u16) -> MomentumGlowConfig {
    let dark = page == Page::Dark;
    MomentumGlowConfig {
        enabled: true,
        intensity: 1.0,
        tau_s: TAU,
        radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
        base: 0x0050_FA7B,
        dark_theme: dark,
        hue: 0x0050_FA7B,
        theme_fg: if dark { 0x00C8_C8D2 } else { 0x0024_292F },
        theme_bg: if dark { 0x0010_1016 } else { 0x00FF_FFFF },
        block: true,
        glyph_span: Some((0, i32::from(col))),
    }
}

/// One host call, at a story offset (ns from the origin).
#[derive(Clone, Copy)]
enum Call {
    Key {
        at: u64,
    },
    Reset,
    Tick {
        at: u64,
        cur: (u16, u16),
        cfg: MomentumGlowConfig,
    },
    Query {
        at: u64,
    },
}

const KEY: usize = 0;
const RESET: usize = 1;
const TICK: usize = 2;
const QUERY: usize = 3;
const CALL_NAMES: [&str; 4] = ["key", "reset", "tick", "query"];

/// What the host calls, in the shape every path takes. No `Deref` hides the host's own.
trait Glow {
    fn key(&mut self, now: Instant, tau: f32);
    fn rest(&mut self);
    fn frame(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> MomentumGlowFrame;
    fn active(&self) -> bool;
    fn settles(&self) -> bool;
    fn wake(&mut self, now: Instant, lane: Duration) -> Option<Instant>;
}

impl Glow for MomentumGlow {
    #[inline]
    fn key(&mut self, now: Instant, tau: f32) {
        self.on_key(now, tau);
    }

    #[inline]
    fn rest(&mut self) {
        self.reset();
    }

    #[inline]
    fn frame(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> MomentumGlowFrame {
        self.tick(cur, now, geom, cfg, out, under)
    }

    #[inline]
    fn active(&self) -> bool {
        self.is_active()
    }

    #[inline]
    fn settles(&self) -> bool {
        self.settling()
    }

    #[inline]
    fn wake(&mut self, now: Instant, lane: Duration) -> Option<Instant> {
        self.next_change_deadline(now, lane)
    }
}

impl Glow for MomentumHost {
    #[inline]
    fn key(&mut self, now: Instant, tau: f32) {
        self.on_key(now, tau);
    }

    #[inline]
    fn rest(&mut self) {
        self.reset();
    }

    #[inline]
    fn frame(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> MomentumGlowFrame {
        self.tick(cur, now, geom, cfg, out, under)
    }

    #[inline]
    fn active(&self) -> bool {
        self.is_active()
    }

    #[inline]
    fn settles(&self) -> bool {
        self.settling()
    }

    #[inline]
    fn wake(&mut self, now: Instant, lane: Duration) -> Option<Instant> {
        self.next_change_deadline(now, lane)
    }
}

/// FNV-1a over everything a path shows the host: frames, quads, wakes, resets.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn quads(&mut self, qs: &[GlowQuad]) {
        self.u64(qs.len() as u64);
        for q in qs {
            self.u64(
                u64::from(q.row)
                    | (u64::from(q.x) << 16)
                    | (u64::from(q.y) << 32)
                    | (u64::from(q.w) << 48),
            );
            self.u64(u64::from(q.h) | (u64::from(q.color) << 16) | (u64::from(q.alpha) << 48));
            self.u64(u64::from(q.color2) | (u64::from(q.alpha2) << 32));
        }
    }

    fn frame(&mut self, f: MomentumGlowFrame, out: &[GlowQuad], under: &[GlowQuad]) {
        self.u64(f.fill.map_or(u64::MAX, u64::from));
        self.u64(u64::from(f.hot));
        self.u64(f.fp);
        self.quads(out);
        self.quads(under);
    }

    fn wake(&mut self, at: Option<u64>) {
        self.u64(at.unwrap_or(u64::MAX));
    }
}

/// The story offset of `t`.
fn offset(t0: Instant, t: Instant) -> u64 {
    nanos(t.saturating_duration_since(t0))
}

fn at(t0: Instant, offset: u64) -> Instant {
    t0 + Duration::from_nanos(offset)
}

/// The first frame boundary after `t`.
fn vsync_after(t: u64) -> u64 {
    (t / LANE_NS + 1) * LANE_NS
}

/// What one drive of the story did.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Shape {
    keys: u64,
    resets: u64,
    ticks: u64,
    queries: u64,
    /// The lane parked with the glow cold.
    parks: u64,
    /// The lane parked with the glow still settling: light stranded on glass.
    stranded: u64,
    /// The quads the ticks appended, both streams, and the most one tick appended.
    quads: u64,
    max_quads: u64,
    /// The story's length: its last offset plus the final idle.
    span_ns: u64,
}

/// Runs the story against `g` closed loop with the host's frame lane, recording each call.
fn drive(
    g: &mut impl Glow,
    t0: Instant,
    calls: &mut Vec<Call>,
    fnv: &mut Fnv,
) -> Result<Shape, String> {
    let mut shape = Shape::default();
    let (mut out, mut under) = (Vec::with_capacity(QUADS), Vec::with_capacity(QUADS));
    let mut now = 0u64;
    let mut next_frame: Option<u64> = None;
    let (mut row, mut col) = (2u16, 4u16);
    let mut page = Page::Dark;
    for (seg_page, rate, cool) in SEGMENTS {
        if seg_page != page {
            g.rest();
            calls.push(Call::Reset);
            fnv.u64(RESET_TAG);
            shape.resets += 1;
            page = seg_page;
            row = (row + 2) % ROWS;
            col = 4;
        }
        let gap = 1_000_000_000 / rate;
        let start = now;
        let mut next_key = now;
        let mut typed = 0u16;
        let mut cut_at = None;
        loop {
            let key = if typed < KEYS_PER_BURST {
                Some(next_key)
            } else {
                None
            };
            let (t, is_key) = match (key, next_frame) {
                (Some(k), Some(f)) if f < k => (f, false),
                (Some(k), _) => (k, true),
                (None, Some(f)) => (f, false),
                (None, None) => break,
            };
            if let Some(cut) = cut_at
                && t > cut
            {
                now = cut;
                break;
            }
            if t - start > HANG_NS {
                return Err("the story's lane never parked (a minute of story time)".into());
            }
            now = t;
            if is_key {
                g.key(at(t0, t), TAU);
                calls.push(Call::Key { at: t });
                shape.keys += 1;
                typed += 1;
                next_key = t + gap;
                if typed == KEYS_PER_BURST && matches!(cool, Cool::Cut) {
                    cut_at = Some(t + CUT_AFTER_NS);
                }
                col += 1;
                if col >= COLS - 1 {
                    col = 4;
                    row = (row + 1) % ROWS;
                }
                let v = vsync_after(t);
                next_frame = Some(next_frame.map_or(v, |f| f.min(v)));
                continue;
            }
            let cfg = config(page, col);
            let cur = (row, col);
            out.clear();
            under.clear();
            let fr = g.frame(Some(cur), at(t0, t), geom(), &cfg, &mut out, &mut under);
            calls.push(Call::Tick { at: t, cur, cfg });
            fnv.frame(fr, &out, &under);
            shape.ticks += 1;
            let n = (out.len() + under.len()) as u64;
            shape.quads += n;
            shape.max_quads = shape.max_quads.max(n);
            next_frame = if g.active() {
                Some(vsync_after(t))
            } else if g.settles() {
                let w = g.wake(at(t0, t), LANE).map(|w| offset(t0, w));
                calls.push(Call::Query { at: t });
                fnv.wake(w);
                shape.queries += 1;
                match w {
                    Some(w) if w > t => Some(w),
                    Some(w) => {
                        return Err(format!("a wake at {w} ns, not after the frame at {t} ns"));
                    }
                    None => {
                        shape.stranded += 1;
                        None
                    }
                }
            } else {
                shape.parks += 1;
                None
            };
        }
        if matches!(cool, Cool::Park) {
            now += IDLE_AFTER_PARK_NS;
        }
    }
    shape.span_ns = now;
    Ok(shape)
}

/// The story, as calls and as the events an attached host records for them.
struct Story {
    t0: Instant,
    calls: Vec<Call>,
    events: Vec<MomentumEvent>,
    shape: Shape,
    digest: u64,
}

impl Story {
    fn generate(t0: Instant) -> Result<Story, String> {
        let mut calls = Vec::new();
        let mut fnv = Fnv::new();
        let shape = drive(&mut MomentumGlow::default(), t0, &mut calls, &mut fnv)?;
        let events = calls.iter().map(event).collect();
        Ok(Story {
            t0,
            calls,
            events,
            shape,
            digest: fnv.0,
        })
    }

    fn json(&self) -> Obj {
        let s = self.shape;
        Obj::new()
            .u("events", self.calls.len() as u64)
            .u("keys", s.keys)
            .u("resets", s.resets)
            .u("ticks", s.ticks)
            .u("queries", s.queries)
            .u("parks", s.parks)
            .u("stranded", s.stranded)
            .f("quads_per_tick", ratio(s.quads, s.ticks))
            .u("max_quads_per_tick", s.max_quads)
            .u("span_ns", s.span_ns)
            .t("digest", &format!("{:016x}", self.digest))
            .t(
                "bursts",
                "24 keys each at 3,6,10,16 keys/s, dark page then light; cool-downs park,cut,park,cut | park,cut,park,park; one reset between pages",
            )
            .t(
                "geometry",
                "cells 15x28 px, grid 24x100, origin (24,84), window 1548x780, head 80",
            )
            .u("frame_interval_ns", LANE_NS)
    }
}

/// The event an attached host records for `call`.
fn event(call: &Call) -> MomentumEvent {
    match *call {
        Call::Key { at } => MomentumEvent::Key {
            at: i128::from(at),
            tau_bits: TAU.to_bits(),
        },
        Call::Reset => MomentumEvent::Reset,
        Call::Tick { at, cur, cfg } => MomentumEvent::Tick {
            cur: Some(cur),
            at: i128::from(at),
            geom: GeomRecord::from(&geom()),
            cfg: ConfigRecord::from(&cfg),
        },
        Call::Query { at } => MomentumEvent::Query {
            at: i128::from(at),
            frame_interval: LANE,
        },
    }
}

/// Which call an event records.
fn call_kind(e: &MomentumEvent) -> usize {
    match e {
        MomentumEvent::Key { .. } => KEY,
        MomentumEvent::Reset => RESET,
        MomentumEvent::Tick { .. } => TICK,
        MomentumEvent::Query { .. } => QUERY,
    }
}

trait Meter {
    fn time<R>(&mut self, call: usize, f: impl FnOnce() -> R) -> R;
}

/// No clock reads: throughput passes.
struct Free;

impl Meter for Free {
    #[inline(always)]
    fn time<R>(&mut self, _: usize, f: impl FnOnce() -> R) -> R {
        f()
    }
}

/// Two clock reads around each call.
struct PerCall([Vec<u64>; 4]);

impl PerCall {
    fn new() -> PerCall {
        PerCall(Default::default())
    }

    fn reserve(&mut self, story: &Story) {
        let s = story.shape;
        for (v, n) in self
            .0
            .iter_mut()
            .zip([s.keys, s.resets, s.ticks, s.queries])
        {
            v.reserve(usize::try_from(n).unwrap_or(0));
        }
    }
}

impl Meter for PerCall {
    #[inline(always)]
    fn time<R>(&mut self, call: usize, f: impl FnOnce() -> R) -> R {
        let (r, ns) = timed(f);
        self.0[call].push(ns);
        r
    }
}

struct Bufs {
    out: Vec<GlowQuad>,
    under: Vec<GlowQuad>,
}

impl Bufs {
    fn new() -> Bufs {
        Bufs {
            out: Vec::with_capacity(QUADS),
            under: Vec::with_capacity(QUADS),
        }
    }
}

/// Plays the recorded calls into `g`.
fn play<M: Meter>(
    g: &mut impl Glow,
    t0: Instant,
    calls: &[Call],
    bufs: &mut Bufs,
    meter: &mut M,
    mut fnv: Option<&mut Fnv>,
) {
    let geom = geom();
    for call in calls {
        match *call {
            Call::Key { at: t } => {
                let now = at(t0, t);
                meter.time(KEY, || g.key(now, TAU));
            }
            Call::Reset => {
                meter.time(RESET, || g.rest());
                if let Some(f) = fnv.as_deref_mut() {
                    f.u64(RESET_TAG);
                }
            }
            Call::Tick { at: t, cur, cfg } => {
                let now = at(t0, t);
                bufs.out.clear();
                bufs.under.clear();
                let (out, under) = (&mut bufs.out, &mut bufs.under);
                let fr = meter.time(TICK, || {
                    let fr = g.frame(Some(cur), now, geom, &cfg, out, under);
                    black_box((g.active(), g.settles()));
                    fr
                });
                if let Some(f) = fnv.as_deref_mut() {
                    f.frame(fr, &bufs.out, &bufs.under);
                }
                black_box(fr);
            }
            Call::Query { at: t } => {
                let now = at(t0, t);
                let w = meter.time(QUERY, || g.wake(now, LANE));
                if let Some(f) = fnv.as_deref_mut() {
                    f.wake(w.map(|w| offset(t0, w)));
                }
                black_box(w);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Direct,
    Host,
    Attached,
}

impl PathKind {
    const ALL: [PathKind; 3] = [PathKind::Direct, PathKind::Host, PathKind::Attached];

    fn name(self) -> &'static str {
        match self {
            PathKind::Direct => "direct",
            PathKind::Host => "host",
            PathKind::Attached => "attached",
        }
    }
}

/// One pass of the story through a fresh path.
struct Pass {
    /// `aos_attach`'s time (the attached path), outside the pass's interval.
    attach_ns: Option<u64>,
    /// The play's wall time and counters: from just before the first call to just after the
    /// last.
    wall_ns: u64,
    count: Count,
    host: Option<MomentumHost>,
}

fn pass<M: Meter>(
    kind: PathKind,
    story: &Story,
    bufs: &mut Bufs,
    meter: &mut M,
    fnv: Option<&mut Fnv>,
) -> Pass {
    let t0 = story.t0;
    match kind {
        PathKind::Direct => {
            let mut g = MomentumGlow::default();
            let ((), wall_ns, count) =
                measured(|| play(&mut g, t0, &story.calls, bufs, meter, fnv));
            black_box(&g);
            Pass {
                attach_ns: None,
                wall_ns,
                count,
                host: None,
            }
        }
        PathKind::Host | PathKind::Attached => {
            let mut h = MomentumHost::default();
            let attach_ns = (kind == PathKind::Attached)
                .then(|| timed(|| h.aos_attach(t0, MomentumCode::Shipped, BUILD, 1)).1);
            let ((), wall_ns, count) =
                measured(|| play(&mut h, t0, &story.calls, bufs, meter, fnv));
            Pass {
                attach_ns,
                wall_ns,
                count,
                host: Some(h),
            }
        }
    }
}

struct Calibration {
    empty: Count,
    json: Obj,
}

/// The clock's step and a read pair's cost, and the counters' own cost over an empty
/// interval (the median of 31).
fn calibrate() -> Calibration {
    let mut min_step = u64::MAX;
    let mut prev = Instant::now();
    for _ in 0..200_000 {
        let now = Instant::now();
        let d = nanos(now.duration_since(prev));
        if d > 0 {
            min_step = min_step.min(d);
        }
        prev = now;
    }
    let mut pairs: Vec<u64> = (0..20_000).map(|_| timed(|| ()).1).collect();
    let empties: Vec<Count> = (0..31).map(|_| measured(|| ()).2).collect();
    let empty = Count::median(&empties);
    let mut walls: Vec<u64> = (0..31).map(|_| measured(|| ()).1).collect();
    let tb = os::timebase();
    let json = Obj::new()
        .u("clock_step_min_ns", min_step)
        .o("clock_pair_ns", dist(&mut pairs))
        .t(
            "timebase",
            &tb.map_or_else(|| "unavailable".into(), |(n, d)| format!("{n}/{d}")),
        )
        .o("empty_interval", empty.json())
        .o("empty_interval_wall_ns", dist(&mut walls))
        .b("rusage", os::rusage().is_some())
        .b("task_events", os::task_events().is_some())
        .b("thread_cpu", os::thread_cpu_ns() > 0);
    Calibration { empty, json }
}

fn machine() -> Obj {
    let s = |k: &str| os::sysctl_str(k).unwrap_or_else(|| "unavailable".into());
    Obj::new()
        .t("hw.model", &s("hw.model"))
        .t("cpu", &s("machdep.cpu.brand_string"))
        .t("macos", &s("kern.osproductversion"))
        .t("build", &s("kern.osversion"))
        .opt_u("hw.ncpu", os::sysctl_u64("hw.ncpu"))
        .opt_u(
            "perflevel0.physicalcpu",
            os::sysctl_u64("hw.perflevel0.physicalcpu"),
        )
        .opt_u(
            "perflevel1.physicalcpu",
            os::sysctl_u64("hw.perflevel1.physicalcpu"),
        )
        .opt_u("hw.memsize", os::sysctl_u64("hw.memsize"))
}

fn load() -> String {
    os::load().map_or_else(
        || "unavailable".into(),
        |[a, b, c]| format!("{a:.2} {b:.2} {c:.2}"),
    )
}

fn footprint() -> Obj {
    let r = os::rusage();
    Obj::new()
        .opt_u("phys_footprint", r.as_ref().map(|r| r.footprint))
        .opt_u(
            "lifetime_max_phys_footprint",
            r.as_ref().map(|r| r.lifetime_max_footprint),
        )
        .u("heap_live_bytes", live_bytes())
        .u("heap_peak_live_bytes", PEAK.load(Relaxed))
}

const WARMUP_PASSES: usize = 5;
const TIMED_PASSES: usize = 30;
const UNTIMED_PASSES: usize = 100;
const OP_REPEATS: usize = 50;
const REPLAY_REPEATS: usize = 30;
const REPAIR_REPEATS: usize = 30;

fn fail(why: impl std::fmt::Display) -> ! {
    eprintln!("aos_harness: FAILED: {why}");
    std::process::exit(1);
}

/// The three paths: a digest check each, warm-up, timed passes and untimed passes, round
/// robin.
fn measure_paths(story: &Story, cal: &Calibration) -> Result<(Obj, MomentumHost), String> {
    let mut bufs = Bufs::new();
    let mut checked = None;
    for kind in PathKind::ALL {
        let mut fnv = Fnv::new();
        let p = pass(kind, story, &mut bufs, &mut Free, Some(&mut fnv));
        if fnv.0 != story.digest {
            return Err(format!(
                "the {} path showed the host something else than the story: digest {:016x}, the story's {:016x}",
                kind.name(),
                fnv.0,
                story.digest
            ));
        }
        if kind == PathKind::Attached {
            checked = p.host;
        }
    }
    let checked = checked.ok_or("the attached path kept no host")?;
    let mut held = Obj::new();
    for kind in PathKind::ALL {
        let before = live_bytes();
        let p = pass(kind, story, &mut bufs, &mut Free, None);
        held = held.u(kind.name(), live_bytes().saturating_sub(before));
        drop(p);
    }
    for _ in 0..WARMUP_PASSES {
        for kind in PathKind::ALL {
            black_box(pass(kind, story, &mut bufs, &mut Free, None).wall_ns);
        }
    }
    let mut per = [PerCall::new(), PerCall::new(), PerCall::new()];
    let (mut attach, mut detach) = (Vec::new(), Vec::new());
    for _ in 0..TIMED_PASSES {
        for (i, kind) in PathKind::ALL.into_iter().enumerate() {
            per[i].reserve(story);
            let p = pass(kind, story, &mut bufs, &mut per[i], None);
            attach.extend(p.attach_ns);
            if kind == PathKind::Attached
                && let Some(mut h) = p.host
            {
                detach.push(timed(|| h.aos_detach()).1);
            }
        }
    }
    let mut tally = [Count::default(); 3];
    let mut walls: [Vec<u64>; 3] = Default::default();
    for _ in 0..UNTIMED_PASSES {
        for (i, kind) in PathKind::ALL.into_iter().enumerate() {
            let p = pass(kind, story, &mut bufs, &mut Free, None);
            tally[i].add(p.count);
            walls[i].push(p.wall_ns);
        }
    }
    let events = story.calls.len() as u64;
    let mut paths = Obj::new();
    for (i, kind) in PathKind::ALL.into_iter().enumerate() {
        let mut calls = Obj::new();
        for (c, name) in CALL_NAMES.iter().enumerate() {
            calls = calls.o(name, dist(&mut per[i].0[c]));
        }
        let wall: u64 = walls[i].iter().sum();
        let all_events = events * UNTIMED_PASSES as u64;
        let net = tally[i].net(cal.empty, UNTIMED_PASSES as u64);
        paths = paths.o(
            kind.name(),
            Obj::new()
                .o("per_call_ns", calls)
                .u("timed_passes", TIMED_PASSES as u64)
                .u("untimed_passes", UNTIMED_PASSES as u64)
                .o("untimed_pass_ns", dist(&mut walls[i]))
                .f("ns_per_event", ratio(wall, all_events))
                .f("events_per_s", ratio(all_events, wall) * 1e9)
                .f("cpu_ns_per_event", ratio(net.0[CPU_NS], all_events))
                .f("ghz", ratio(net.0[CYCLES], net.0[CPU_NS]))
                .o("per_event", net.per(all_events))
                .o("untimed_total", net.json()),
        );
    }
    let status = checked.aos_status();
    let report = Obj::new()
        .o("paths", paths)
        .o("heap_held_after_story_bytes", held)
        .o("attach_ns", dist(&mut attach))
        .o("detach_ns", dist(&mut detach))
        .o(
            "attached_status",
            Obj::new()
                .u("log_len", status.log_len as u64)
                .u("reanchors", status.stats.reanchors)
                .u("breaks", status.stats.breaks)
                .t("state", status.state_digest.as_deref().unwrap_or("none"))
                .t(
                    "chain",
                    status.observation_chain.as_deref().unwrap_or("none"),
                ),
        );
    Ok((report, checked))
}

/// The attached host's trace, decoded: identity, ancestor state, log; the log must be the
/// story's events, recorded once each.
struct Decoded {
    identity: TaskIdentity,
    c0: Checkpoint,
    s0: MomentumState,
    log: EventLog<MomentumSchema>,
    trace_checkpoint_bytes: usize,
    trace_log_bytes: Vec<u8>,
}

fn decode_trace(
    story: &Story,
    host: &MomentumHost,
    schema: &MomentumSchema,
) -> Result<Decoded, String> {
    let trace = host.aos_trace().map_err(|e| e.to_string())?;
    let c0 = Checkpoint::decode(&trace.checkpoint).map_err(|e| e.to_string())?;
    let s0 = c0
        .restore(schema, &trace.identity)
        .map_err(|e| e.to_string())?;
    let (log, tail) = EventLog::<MomentumSchema>::decode(
        schema,
        &trace.log,
        &trace.identity,
        &c0.digest(),
        MOMENTUM_MAX_EVENTS,
    )
    .map_err(|e| e.to_string())?;
    if tail != Tail::Clean {
        return Err(format!("the exported log ended {tail:?}"));
    }
    if log.inputs() != story.events.as_slice() {
        return Err(format!(
            "the attached host recorded {} inputs that are not the story's {} events",
            log.len(),
            story.events.len()
        ));
    }
    Ok(Decoded {
        identity: trace.identity,
        c0,
        s0,
        log,
        trace_checkpoint_bytes: trace.checkpoint.len(),
        trace_log_bytes: trace.log,
    })
}

fn state_bytes(schema: &MomentumSchema, s: &MomentumState) -> Vec<u8> {
    let mut e = Enc::new();
    schema.encode_state(s, &mut e);
    e.finish()
}

/// Capture, encode, decode and restore at every state the story passes through (each one
/// stepped by the shipped code from the attach checkpoint), the bytes each event changes, and
/// what retained checkpoints hold.
fn measure_checkpoints(story: &Story, schema: &MomentumSchema, d: &Decoded) -> Result<Obj, String> {
    let code = PrebuiltMomentum::new(MomentumCode::Shipped, BUILD);
    let mut states = Vec::with_capacity(d.log.len());
    let mut s = d.s0;
    for input in d.log.inputs() {
        code.step(schema, &mut s, input, &mut NoEffects)
            .map_err(|e| e.to_string())?;
        states.push(s);
    }
    let first = d.log.header().first_cursor;
    let base = d.c0.provenance().clone();
    let parent = Some(d.c0.digest());
    let prov = |i: usize| Provenance {
        input_cursor: first + i as u64 + 1,
        parent,
        ..base.clone()
    };
    let (mut capture, mut encode, mut decode, mut restore) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut ck_min, mut ck_max) = (usize::MAX, 0usize);
    let mut changed = Vec::with_capacity(states.len());
    let mut prev = state_bytes(schema, &d.s0);
    let (mut st_min, mut st_max) = (usize::MAX, 0usize);
    for (i, s) in states.iter().enumerate() {
        let p = prov(i);
        let (c, t) = timed(|| Checkpoint::capture(schema, s, p));
        capture.push(t);
        let (bytes, t) = timed(|| c.encode());
        encode.push(t);
        let bytes = bytes.map_err(|e| e.to_string())?;
        let (back, t) = timed(|| Checkpoint::decode(&bytes));
        decode.push(t);
        let back = back.map_err(|e| e.to_string())?;
        let (st, t) = timed(|| back.restore(schema, &d.identity));
        restore.push(t);
        let st = st.map_err(|e| e.to_string())?;
        if state_digest(schema, &st) != c.state_digest() {
            return Err(format!(
                "the checkpoint at input {i} restored another state"
            ));
        }
        ck_min = ck_min.min(bytes.len());
        ck_max = ck_max.max(bytes.len());
        let now = state_bytes(schema, s);
        st_min = st_min.min(now.len());
        st_max = st_max.max(now.len());
        let differ =
            now.iter().zip(&prev).filter(|(a, b)| a != b).count() + now.len().abs_diff(prev.len());
        changed.push(differ as u64);
        prev = now;
    }
    let snapshot_len =
        d.s0.glow()
            .snapshot(story.t0)
            .map_err(|e| format!("{e:?}"))?
            .encode()
            .len();
    let mut retained = Vec::new();
    for k in [1usize, 16, 256, 4096] {
        let before = live_bytes();
        let kept: Vec<Vec<u8>> = (0..k)
            .map(|i| Checkpoint::capture(schema, &states[i % states.len()], prov(i)).encode())
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let encoded = live_bytes().saturating_sub(before);
        drop(kept);
        let before = live_bytes();
        let held: Vec<Checkpoint> = (0..k)
            .map(|i| Checkpoint::capture(schema, &states[i % states.len()], prov(i)))
            .collect();
        let captured = live_bytes().saturating_sub(before);
        black_box(&held);
        drop(held);
        retained.push(
            Obj::new()
                .u("checkpoints", k as u64)
                .u("encoded_live_bytes", encoded)
                .f("encoded_bytes_each", ratio(encoded, k as u64))
                .u("captured_live_bytes", captured)
                .f("captured_bytes_each", ratio(captured, k as u64)),
        );
    }
    Ok(Obj::new()
        .u("states", states.len() as u64)
        .o("capture_ns", dist(&mut capture))
        .o("encode_ns", dist(&mut encode))
        .o("decode_ns", dist(&mut decode))
        .o("restore_ns", dist(&mut restore))
        .u("engine_snapshot_bytes", snapshot_len as u64)
        .u("state_bytes_min", st_min as u64)
        .u("state_bytes_max", st_max as u64)
        .u("checkpoint_bytes_min", ck_min as u64)
        .u("checkpoint_bytes_max", ck_max as u64)
        .u("attach_checkpoint_bytes", d.trace_checkpoint_bytes as u64)
        .o("state_bytes_changed_per_event", dist(&mut changed))
        .a("retained", retained))
}

/// A byte-vector state of any size: what a checkpoint costs as the state grows.
struct Blob {
    id: ArtifactId,
}

impl Schema for Blob {
    type State = Vec<u8>;
    type Input = u8;
    type Observation = u8;

    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn encode_state(&self, s: &Self::State, e: &mut Enc) {
        e.bytes(s);
    }

    fn decode_state(&self, d: &mut Dec<'_>) -> Result<Self::State, WireFault> {
        Ok(d.bytes("blob.state")?.to_vec())
    }

    fn encode_input(&self, i: &u8, e: &mut Enc) {
        e.u8(*i);
    }

    fn decode_input(&self, d: &mut Dec<'_>) -> Result<u8, WireFault> {
        d.u8("blob.input")
    }

    fn encode_observation(&self, o: &u8, e: &mut Enc) {
        e.u8(*o);
    }

    fn paused_class(&self, _: &u8) -> PausedClass {
        PausedClass::Skip
    }
}

/// Checkpoint cost at 64 B, 4 KiB, 256 KiB and 4 MiB of state, with one byte changed
/// between checkpoints and with every byte changed.
fn measure_state_sizes(base: &Provenance) -> Result<Vec<Obj>, String> {
    let blob = Blob {
        id: ArtifactId::new(ArtifactKind::Schema, "aos-harness.blob", 1, b"bytes"),
    };
    let artifact = |kind, name: &str| ArtifactId::new(kind, name, 1, b"aos-harness");
    let identity = TaskIdentity {
        task: artifact(ArtifactKind::Task, "aos-harness.blob"),
        code: artifact(ArtifactKind::Code, "aos-harness.blob"),
        contract: artifact(ArtifactKind::Contract, "aos-harness.blob"),
        schema: blob.id.clone(),
        profile: artifact(ArtifactKind::Profile, "aos-harness.blob"),
    };
    let prov = Provenance {
        identity: identity.clone(),
        parent: None,
        ..base.clone()
    };
    let mut rows = Vec::new();
    for size in [64usize, 4096, 256 * 1024, 4 * 1024 * 1024] {
        let repeats = match size {
            0..=4096 => 1000,
            4097..=262_144 => 100,
            _ => 12,
        };
        for every in [false, true] {
            let mut state = vec![0xA5u8; size];
            let (mut capture, mut encode, mut decode, mut restore) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let mut bytes_len = 0usize;
            for r in 0..repeats {
                if every {
                    state.fill(r as u8);
                } else {
                    state[r % size] ^= 0xFF;
                }
                let p = prov.clone();
                let (c, t) = timed(|| Checkpoint::capture(&blob, &state, p));
                capture.push(t);
                let (bytes, t) = timed(|| c.encode());
                encode.push(t);
                let bytes = bytes.map_err(|e| e.to_string())?;
                bytes_len = bytes.len();
                let (back, t) = timed(|| Checkpoint::decode(&bytes));
                decode.push(t);
                let back = back.map_err(|e| e.to_string())?;
                let (st, t) = timed(|| back.restore(&blob, &identity));
                restore.push(t);
                if st.map_err(|e| e.to_string())? != state {
                    return Err(format!("a {size}-byte blob restored other bytes"));
                }
            }
            rows.push(
                Obj::new()
                    .u("state_bytes", size as u64)
                    .t("changed", if every { "every byte" } else { "one byte" })
                    .u("checkpoint_bytes", bytes_len as u64)
                    .o("capture_ns", dist(&mut capture))
                    .o("encode_ns", dist(&mut encode))
                    .o("decode_ns", dist(&mut decode))
                    .o("restore_ns", dist(&mut restore)),
            );
        }
    }
    Ok(rows)
}

/// The event log: encode, decode, trace digest, and its bytes on disk and in memory.
fn measure_log(schema: &MomentumSchema, d: &Decoded, cal: &Calibration) -> Result<Obj, String> {
    let log = &d.log;
    let n = log.len() as u64;
    let first = log.header().first_cursor;
    let end = log.end_cursor();
    let ancestor = d.c0.digest();
    let (mut enc, mut dec, mut dig) = (Vec::new(), Vec::new(), Vec::new());
    let mut count = Count::default();
    for _ in 0..OP_REPEATS {
        let (bytes, t) = timed(|| log.encode(schema));
        enc.push(t);
        if bytes.map_err(|e| e.to_string())? != d.trace_log_bytes {
            return Err("the log re-encoded to other bytes than the host exported".into());
        }
        let (back, t, c) = measured(|| {
            EventLog::<MomentumSchema>::decode(
                schema,
                &d.trace_log_bytes,
                &d.identity,
                &ancestor,
                MOMENTUM_MAX_EVENTS,
            )
        });
        dec.push(t);
        count.add(c);
        back.map_err(|e| e.to_string())?;
        let (digest, t) = timed(|| log.trace_digest(schema, first, end));
        dig.push(t);
        digest.map_err(|e| e.to_string())?;
    }
    let decode_net = count.net(cal.empty, OP_REPEATS as u64);
    let header_bytes = EventLog::<MomentumSchema>::new(log.header().clone(), MOMENTUM_MAX_EVENTS)
        .encode(schema)
        .map_err(|e| e.to_string())?
        .len();
    let mut kinds = Obj::new();
    for (kind, name) in CALL_NAMES.iter().enumerate() {
        let Some(found) = log.inputs().iter().find(|e| call_kind(e) == kind) else {
            continue;
        };
        let mut e = Enc::new();
        schema.encode_input(found, &mut e);
        let mut one = EventLog::<MomentumSchema>::new(log.header().clone(), MOMENTUM_MAX_EVENTS);
        one.push(*found).map_err(|e| e.to_string())?;
        let framed = one.encode(schema).map_err(|e| e.to_string())?.len() - header_bytes;
        kinds = kinds.o(
            name,
            Obj::new()
                .u("encoded_bytes", e.len() as u64)
                .u("record_bytes", framed as u64),
        );
    }
    let before = live_bytes();
    let held = EventLog::<MomentumSchema>::decode(
        schema,
        &d.trace_log_bytes,
        &d.identity,
        &ancestor,
        MOMENTUM_MAX_EVENTS,
    )
    .map_err(|e| e.to_string())?;
    let decoded_live = live_bytes().saturating_sub(before);
    black_box(&held);
    drop(held);
    let total = d.trace_log_bytes.len() as u64;
    let mut dec_rate = dec.clone();
    dec_rate.sort_unstable();
    let p50 = dec_rate.get(dec_rate.len() / 2).copied().unwrap_or(0);
    Ok(Obj::new()
        .u("events", n)
        .u("encoded_bytes", total)
        .u("header_bytes", header_bytes as u64)
        .f("bytes_per_event", ratio(total - header_bytes as u64, n))
        .o("per_kind", kinds)
        .u(
            "in_memory_input_bytes",
            n * std::mem::size_of::<MomentumEvent>() as u64,
        )
        .u("size_of_event", std::mem::size_of::<MomentumEvent>() as u64)
        .u("decoded_live_bytes", decoded_live)
        .o("encode_ns", dist(&mut enc))
        .o("decode_ns", dist(&mut dec))
        .o("trace_digest_ns", dist(&mut dig))
        .f("decode_events_per_s_at_p50", ratio(n, p50) * 1e9)
        .o("decode_per_event", decode_net.per(n * OP_REPEATS as u64)))
}

/// Shadow replay of the shipped code over the recorded log from the attach checkpoint,
/// checked against the live state digest and observation chain.
fn measure_replay(
    schema: &MomentumSchema,
    d: &Decoded,
    host: &MomentumHost,
    cal: &Calibration,
) -> Result<Obj, String> {
    let code = PrebuiltMomentum::new(MomentumCode::Shipped, BUILD);
    let status = host.aos_status();
    let first = d.log.header().first_cursor;
    let n = d.log.len() as u64;
    let mut runs = Vec::new();
    let mut count = Count::default();
    for _ in 0..REPLAY_REPEATS {
        let mut port = ShadowPort::new(&[]);
        let (run, t, c) =
            measured(|| replay(schema, &code, d.s0, d.log.inputs(), first, &mut port));
        runs.push(t);
        count.add(c);
        port.finish().map_err(|e| e.to_string())?;
        if let Some((cursor, why)) = &run.refused {
            return Err(format!("the replay refused input {cursor}: {why}"));
        }
        if Some(run.state_digest.short()) != status.state_digest
            || Some(run.observation_chain.short()) != status.observation_chain
        {
            return Err("the replay did not land on the live state and chain".into());
        }
        black_box(run);
    }
    let net = count.net(cal.empty, REPLAY_REPEATS as u64);
    let mut port = ShadowPort::new(&[]);
    let run = replay(schema, &code, d.s0, d.log.inputs(), first, &mut port);
    let (mut fold, mut obs_bytes) = (Vec::new(), Vec::new());
    let mut chain = empty_chain();
    for (cursor, o) in &run.observations {
        let (len, t) = timed(|| {
            let mut e = Enc::new();
            schema.encode_observation(o, &mut e);
            chain = chain_link(&chain, *cursor, e.as_bytes());
            e.len()
        });
        fold.push(t);
        obs_bytes.push(len as u64);
    }
    if chain != run.observation_chain {
        return Err("the observation fold did not land on the replay's chain".into());
    }
    let fold_total: u64 = fold.iter().sum();
    let mut sorted = runs.clone();
    sorted.sort_unstable();
    let p50 = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
    Ok(Obj::new()
        .u("events", n)
        .o("run_ns", dist(&mut runs))
        .f("events_per_s_at_p50", ratio(n, p50) * 1e9)
        .f("ns_per_event_at_p50", ratio(p50, n))
        .o("per_event", net.per(n * REPLAY_REPEATS as u64))
        .b("matched_live_state_and_chain", true)
        .o("observation_fold_ns", dist(&mut fold))
        .f("observation_fold_ns_per_event", ratio(fold_total, n))
        .o("observation_bytes", dist(&mut obs_bytes)))
}

/// What one repair did: pause and repair times, the witness, the counters around both.
struct Repaired {
    pause_ns: u64,
    repair_ns: u64,
    count: Count,
    live: String,
    candidate: String,
    reproduced: bool,
    replayed: u64,
    log_len: u64,
}

fn verdict(v: &Verdict) -> String {
    match v {
        Verdict::Pass => "Pass".into(),
        Verdict::Fail { cursor, law, .. } => format!("Fail({law} at cursor {cursor})"),
    }
}

/// Pauses `h` at `end` and repairs it to the shipped code.
fn repair(h: &mut MomentumHost, end: Instant) -> Result<Repaired, String> {
    let log_len = h.aos_status().log_len as u64;
    let before = Count::now();
    let (head, pause_ns) = timed(|| h.aos_pause(end, LANE));
    head.map_err(|e| e.to_string())?;
    let (commit, repair_ns) = timed(|| h.aos_repair(MomentumCode::Shipped));
    let count = Count::now().since(before);
    let commit = commit.map_err(|e| e.to_string())?;
    let w = &commit.witness;
    let (from, to) = w.range();
    Ok(Repaired {
        pause_ns,
        repair_ns,
        count,
        live: verdict(w.live_verdict()),
        candidate: verdict(w.candidate_verdict()),
        reproduced: w.same_code_reproduced(),
        replayed: to.saturating_sub(from),
        log_len,
    })
}

fn repairs_json(what: &str, mut done: Vec<Repaired>, cal: &Calibration) -> Obj {
    let mut pause: Vec<u64> = done.iter().map(|r| r.pause_ns).collect();
    let mut rep: Vec<u64> = done.iter().map(|r| r.repair_ns).collect();
    let mut total: Vec<u64> = done.iter().map(|r| r.pause_ns + r.repair_ns).collect();
    let mut count = Count::default();
    for r in &done {
        count.add(r.count);
    }
    let n = done.len() as u64;
    let first = done.swap_remove(0);
    Obj::new()
        .t("what", what)
        .u("repairs", n)
        .u("log_len", first.log_len)
        .u("replayed_inputs", first.replayed)
        .t("live_verdict", &first.live)
        .t("candidate_verdict", &first.candidate)
        .b("same_code_reproduced", first.reproduced)
        .o("pause_ns", dist(&mut pause))
        .o("repair_ns", dist(&mut rep))
        .o("pause_plus_repair_ns", dist(&mut total))
        .o("per_repair", count.net(cal.empty, n).per(n))
}

/// Shipped -> Shipped repairs of the story: each a fresh attached host that played the whole
/// story (untimed), then a pause at the story's end and a repair to the shipped code.
fn measure_shipped_repair(story: &Story, cal: &Calibration) -> Result<Obj, String> {
    let mut bufs = Bufs::new();
    let end = at(story.t0, story.shape.span_ns);
    let mut done = Vec::new();
    for _ in 0..REPAIR_REPEATS {
        let mut h = MomentumHost::default();
        h.aos_attach(story.t0, MomentumCode::Shipped, BUILD, 1);
        play(&mut h, story.t0, &story.calls, &mut bufs, &mut Free, None);
        let r = repair(&mut h, end)?;
        if r.live != "Pass" || r.candidate != "Pass" || !r.reproduced {
            return Err(format!(
                "a Shipped -> Shipped repair's witness read live {} candidate {} reproduced {}",
                r.live, r.candidate, r.reproduced
            ));
        }
        done.push(r);
    }
    Ok(repairs_json(
        "Shipped -> Shipped: pause at the story's end, then prepare (two shadow replays from the attach checkpoint, judged by NoStrandedLight) and commit",
        done,
        cal,
    ))
}

/// The recorded stranded trace: the demonstration fault runs the lane itself (closed loop,
/// attached), so the trace holds the settle queries it answered with no wake; then the
/// repair to the shipped code.
#[cfg(debug_assertions)]
fn measure_stranded_repair(story: &Story, cal: &Calibration) -> Result<Obj, String> {
    let mut done = Vec::new();
    let mut first: Option<(u64, Shape)> = None;
    for _ in 0..REPAIR_REPEATS {
        let mut h = MomentumHost::default();
        h.aos_attach(story.t0, MomentumCode::DemoStrandedSettle, BUILD, 1);
        let mut calls = Vec::new();
        let mut fnv = Fnv::new();
        let shape = drive(&mut h, story.t0, &mut calls, &mut fnv)?;
        match first {
            None => first = Some((fnv.0, shape)),
            Some((digest, s)) if digest == fnv.0 && s == shape => {}
            Some(_) => return Err("the stranded trace differed between two drives".into()),
        }
        let r = repair(&mut h, at(story.t0, shape.span_ns))?;
        if !r.live.starts_with("Fail(HotHasWake") || r.candidate != "Pass" || !r.reproduced {
            return Err(format!(
                "the stranded repair's witness read live {} candidate {} reproduced {}",
                r.live, r.candidate, r.reproduced
            ));
        }
        done.push(r);
    }
    let (digest, shape) = first.ok_or("no stranded drive ran")?;
    Ok(repairs_json(
        "DemoStrandedSettle -> Shipped on the recorded stranded trace: pause at its end, then prepare and commit",
        done,
        cal,
    )
    .o(
        "trace",
        Obj::new()
            .u("keys", shape.keys)
            .u("resets", shape.resets)
            .u("ticks", shape.ticks)
            .u("queries", shape.queries)
            .u("stranded_parks", shape.stranded)
            .u("cold_parks", shape.parks)
            .u("span_ns", shape.span_ns)
            .t("digest", &format!("{digest:016x}")),
    ))
}

const COLD_OPS: [&str; 7] = [
    "direct",
    "host",
    "attached",
    "checkpoint",
    "log",
    "replay",
    "repair",
];

/// OP's first run in this process.
fn cold(op: &str, story: &Story, cal: &Calibration) -> Result<Obj, String> {
    let schema = MomentumSchema::new(story.t0);
    let code = PrebuiltMomentum::new(MomentumCode::Shipped, BUILD);
    let identity = momentum_identity(&code);
    let s0 = MomentumState::new(MomentumGlow::default());
    let mut bufs = Bufs::new();
    let mut ns = Obj::new();
    let mut digests = Obj::new();
    let (setup, count) = match op {
        "direct" | "host" | "attached" => {
            let kind = match op {
                "direct" => PathKind::Direct,
                "host" => PathKind::Host,
                _ => PathKind::Attached,
            };
            let mut per = PerCall::new();
            per.reserve(story);
            let p = pass(kind, story, &mut bufs, &mut per, None);
            ns = ns.opt_u("attach", p.attach_ns);
            for (c, name) in CALL_NAMES.iter().enumerate() {
                ns = ns.opt_u(&format!("first_{name}"), per.0[c].first().copied());
            }
            let mut ticks = per.0[TICK].clone();
            ticks.sort_unstable();
            ns = ns.u("pass_wall", p.wall_ns).u(
                "pass_tick_p50",
                ticks.get(ticks.len() / 2).copied().unwrap_or(0),
            );
            (
                "story generation; the interval is the pass (per-call timed), attach outside it",
                p.count,
            )
        }
        "checkpoint" => {
            let mut s = s0;
            for e in &story.events {
                code.step(&schema, &mut s, e, &mut NoEffects)
                    .map_err(|e| e.to_string())?;
            }
            let prov = Provenance {
                identity: identity.clone(),
                log_epoch: 1,
                input_cursor: story.events.len() as u64,
                effect_cursor: 0,
                branch: aterm_aos::BranchId(0),
                revision: aterm_aos::Revision(1),
                parent: None,
            };
            let before = Count::now();
            let (c, t) = timed(|| Checkpoint::capture(&schema, &s, prov));
            ns = ns.u("capture", t);
            let (bytes, t) = timed(|| c.encode());
            ns = ns.u("encode", t);
            let bytes = bytes.map_err(|e| e.to_string())?;
            let (back, t) = timed(|| Checkpoint::decode(&bytes));
            ns = ns.u("decode", t);
            let back = back.map_err(|e| e.to_string())?;
            let (st, t) = timed(|| back.restore(&schema, &identity));
            ns = ns.u("restore", t);
            st.map_err(|e| e.to_string())?;
            digests = digests
                .t("state", &back.state_digest().hex())
                .t("checkpoint", &back.digest().hex());
            (
                "story generation; the shipped code stepped through every event (Code::step) to the final state; the interval is the four calls",
                Count::now().since(before),
            )
        }
        "log" => {
            let c0 = Checkpoint::capture(&schema, &s0, attach_provenance(&identity));
            let header = LogHeader {
                identity: identity.clone(),
                log_epoch: 1,
                first_cursor: 0,
                ancestor: c0.digest(),
            };
            let mut log = EventLog::<MomentumSchema>::new(header, MOMENTUM_MAX_EVENTS);
            for e in &story.events {
                log.push(*e).map_err(|e| e.to_string())?;
            }
            let before = Count::now();
            let (bytes, t) = timed(|| log.encode(&schema));
            ns = ns.u("encode", t);
            let bytes = bytes.map_err(|e| e.to_string())?;
            let (back, t) = timed(|| {
                EventLog::<MomentumSchema>::decode(
                    &schema,
                    &bytes,
                    &identity,
                    &c0.digest(),
                    MOMENTUM_MAX_EVENTS,
                )
            });
            ns = ns.u("decode", t);
            back.map_err(|e| e.to_string())?;
            let (digest, t) = timed(|| log.trace_digest(&schema, 0, log.end_cursor()));
            ns = ns.u("trace_digest", t);
            digest.map_err(|e| e.to_string())?;
            (
                "story generation; the log built by EventLog::new and push; the interval is the three calls",
                Count::now().since(before),
            )
        }
        "replay" => {
            let before = Count::now();
            let mut port = ShadowPort::new(&[]);
            let (run, t) = timed(|| replay(&schema, &code, s0, &story.events, 0, &mut port));
            let count = Count::now().since(before);
            port.finish().map_err(|e| e.to_string())?;
            if run.refused.is_some() {
                return Err("the cold replay refused an input".into());
            }
            digests = digests
                .t("state", &run.state_digest.hex())
                .t("chain", &run.observation_chain.hex());
            ns = ns
                .u("replay", t)
                .f("replay_ns_per_event", ratio(t, story.events.len() as u64));
            (
                "story generation; the interval is the one replay (the shipped code's first steps in this process)",
                count,
            )
        }
        "repair" => {
            let mut h = MomentumHost::default();
            h.aos_attach(story.t0, MomentumCode::Shipped, BUILD, 1);
            play(&mut h, story.t0, &story.calls, &mut bufs, &mut Free, None);
            let r = repair(&mut h, at(story.t0, story.shape.span_ns))?;
            ns = ns.u("pause", r.pause_ns).u("repair", r.repair_ns);
            (
                "story generation; one attached pass of the story (untimed); the interval is pause and repair",
                r.count,
            )
        }
        other => return Err(format!("unknown cold op `{other}` (one of {COLD_OPS:?})")),
    };
    Ok(Obj::new()
        .t("op", op)
        .t("setup", setup)
        .o("ns", ns)
        .o("digests", digests)
        .f("ghz", {
            let net = count.net(cal.empty, 1);
            if net.0[CPU_NS] < 1_000_000 {
                f64::NAN
            } else {
                ratio(net.0[CYCLES], net.0[CPU_NS])
            }
        })
        .o("interval", count.net(cal.empty, 1).json()))
}

enum Mode {
    Warm,
    Cold(String),
    Stranded,
    Export(String),
    Import(String),
}

fn usage() -> ! {
    eprintln!(
        "usage: aos_harness [--cold OP | --stranded | --export PATH | --import PATH]\n  OP: {}",
        COLD_OPS.join(", ")
    );
    std::process::exit(2);
}

/// The provenance of an attach checkpoint at cursor 0 of log epoch 1, as `aos_attach` takes
/// it on a fresh host.
fn attach_provenance(identity: &TaskIdentity) -> Provenance {
    Provenance {
        identity: identity.clone(),
        log_epoch: 1,
        input_cursor: 0,
        effect_cursor: 0,
        branch: aterm_aos::BranchId(0),
        revision: aterm_aos::Revision(0),
        parent: None,
    }
}

fn counts_json(c: EventCounts) -> Obj {
    Obj::new()
        .u("keys", c.keys as u64)
        .u("resets", c.resets as u64)
        .u("ticks", c.ticks as u64)
        .u("queries", c.queries as u64)
}

/// `--export PATH`: the story played into a host attached under the shipped code, its trace
/// file written to PATH, and the head it records.
fn export(story: &Story, path: &str) -> Result<Obj, String> {
    let mut h = MomentumHost::default();
    h.aos_attach(story.t0, MomentumCode::Shipped, BUILD, 1);
    play(
        &mut h,
        story.t0,
        &story.calls,
        &mut Bufs::new(),
        &mut Free,
        None,
    );
    let schema = MomentumSchema::new(story.t0);
    let decoded = decode_trace(story, &h, &schema)?;
    let trace = h.aos_trace().map_err(|e| e.to_string())?;
    let bytes = trace.encode();
    std::fs::write(path, &bytes).map_err(|e| format!("could not write {path}: {e}"))?;
    Ok(Obj::new()
        .t("path", path)
        .u("file_bytes", bytes.len() as u64)
        .t("checkpoint", &decoded.c0.digest().hex())
        .t("state", &trace.head_state.hex())
        .t("chain", &trace.head_chain.hex())
        .o("counts", counts_json(EventCounts::of(decoded.log.inputs()))))
}

/// `--import PATH`: the trace file decoded on this process's own epoch, replayed from its
/// checkpoint through the shipped code, and what the replay reached.
fn import(story: &Story, path: &str) -> Result<Obj, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {path}: {e}"))?;
    let trace = MomentumTrace::decode(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let epoch = Instant::now();
    let replayed = trace
        .replay(epoch, BUILD)
        .map_err(|e| format!("{path}: {e}"))?;
    let read = trace.read(epoch).map_err(|e| format!("{path}: {e}"))?;
    Ok(Obj::new()
        .t("path", path)
        .u("file_bytes", bytes.len() as u64)
        .t("checkpoint", &replayed.checkpoint.hex())
        .t("recorded_state", &trace.head_state.hex())
        .t("recorded_chain", &trace.head_chain.hex())
        .t("state", &replayed.state_digest.hex())
        .t("chain", &replayed.observation_chain.hex())
        .o("counts", counts_json(replayed.counts))
        .b(
            "log_is_this_process_story",
            read.log.inputs() == story.events.as_slice(),
        ))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match args.as_slice() {
        [] => Mode::Warm,
        [flag, op] if flag == "--cold" && COLD_OPS.contains(&op.as_str()) => Mode::Cold(op.clone()),
        [flag] if flag == "--stranded" => Mode::Stranded,
        [flag, path] if flag == "--export" => Mode::Export(path.clone()),
        [flag, path] if flag == "--import" => Mode::Import(path.clone()),
        _ => usage(),
    };
    aterm_pty::declare_interactive_thread();
    let load_start = load();
    let started = Instant::now();
    let t0 = Instant::now();
    let story = Story::generate(t0).unwrap_or_else(|e| fail(e));
    let cal = calibrate();
    let mut report = Obj::new()
        .t("harness", "aos_harness")
        .t(
            "mode",
            match &mode {
                Mode::Warm => "warm",
                Mode::Cold(_) => "cold",
                Mode::Stranded => "stranded",
                Mode::Export(_) => "export",
                Mode::Import(_) => "import",
            },
        )
        .b("debug_assertions", cfg!(debug_assertions))
        .t(
            "qos",
            "user-interactive (aterm_pty::declare_interactive_thread)",
        )
        .o("machine", machine())
        .t("load_start", &load_start)
        .o("story", story.json());
    report = match mode {
        Mode::Warm => {
            eprintln!(
                "aos_harness: story of {} events; the three paths",
                story.calls.len()
            );
            let (paths, host) = measure_paths(&story, &cal).unwrap_or_else(|e| fail(e));
            let schema = MomentumSchema::new(story.t0);
            let decoded = decode_trace(&story, &host, &schema).unwrap_or_else(|e| fail(e));
            eprintln!("aos_harness: checkpoints");
            let checkpoints =
                measure_checkpoints(&story, &schema, &decoded).unwrap_or_else(|e| fail(e));
            let sizes = measure_state_sizes(decoded.c0.provenance()).unwrap_or_else(|e| fail(e));
            eprintln!("aos_harness: log, replay, repair");
            let log = measure_log(&schema, &decoded, &cal).unwrap_or_else(|e| fail(e));
            let replayed =
                measure_replay(&schema, &decoded, &host, &cal).unwrap_or_else(|e| fail(e));
            let repaired = measure_shipped_repair(&story, &cal).unwrap_or_else(|e| fail(e));
            report
                .o("warm", paths)
                .o("checkpoint", checkpoints)
                .a("state_size_sweep", sizes)
                .o("log", log)
                .o("replay", replayed)
                .o("repair", repaired)
        }
        Mode::Cold(op) => report.o("cold", cold(&op, &story, &cal).unwrap_or_else(|e| fail(e))),
        Mode::Stranded => stranded(report, &story, &cal),
        Mode::Export(path) => report.o("export", export(&story, &path).unwrap_or_else(|e| fail(e))),
        Mode::Import(path) => report.o("import", import(&story, &path).unwrap_or_else(|e| fail(e))),
    };
    let report = report
        .o("calibration", cal.json)
        .o("footprint", footprint())
        .t("load_end", &load())
        .u("run_ns", nanos(started.elapsed()))
        .b("ok", true);
    println!("{}", report.done());
}

#[cfg(debug_assertions)]
fn stranded(report: Obj, story: &Story, cal: &Calibration) -> Obj {
    eprintln!("aos_harness: the stranded trace and its repair; Shipped -> Shipped beside it");
    let stranded = measure_stranded_repair(story, cal).unwrap_or_else(|e| fail(e));
    let shipped = measure_shipped_repair(story, cal).unwrap_or_else(|e| fail(e));
    report
        .o("stranded_repair", stranded)
        .o("shipped_repair", shipped)
}

#[cfg(not(debug_assertions))]
fn stranded(_: Obj, _: &Story, _: &Calibration) -> Obj {
    fail(
        "--stranded needs a build with debug assertions (the demonstration fault exists only there): \
         targo --unverified run --profile opt-test -q -p aterm-bench --example aos_harness -- --stranded",
    )
}
