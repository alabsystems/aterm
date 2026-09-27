// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the stall watchdog can learn about the MAIN thread from another
//! thread: how much CPU it has used, and — at a stall — where it is.
//!
//! # Why the watchdog needs both (the 2026-09-26 freeze)
//!
//! The watchdog's heartbeat is beaten by aterm's own winit roots, so it can say
//! WHICH root ran last but not what the thread is doing now. On 2026-09-26 it
//! reported `no heartbeat for 5.010037459s while inside \`NewEvents\`` — and the
//! main thread was not inside `NewEvents`. That handler had returned; the thread
//! was spinning in CoreFoundation's timer catch-up (`__CFRunLoopDoTimer + 1220`,
//! see `aterm_objc::wake_timer`), below `-[NSApplication run]` with no aterm
//! frame on the stack. Only a macOS hang report and the release's dSYM could
//! say so. Two probes close that:
//!
//! * [`cpu_time`] — the main thread's accumulated CPU (`thread_info`). A
//!   heartbeat that is frozen while the thread burns CPU is a spin, whichever
//!   root ran last — including after `AboutToWait`, the idle park point the
//!   heartbeat rule has to exempt.
//! * [`stack`] — the main thread's return addresses (suspend, read its
//!   registers, walk the frame-pointer chain with `vm_read_overwrite`, resume),
//!   symbolized with `dladdr` only AFTER the thread runs again, so the capture
//!   itself takes no lock the suspended thread could hold. Each frame is
//!   printed as `image + offset` like a hang report — the aterm frames of a
//!   stripped release resolve against its dSYM with `atos -l <load address>`,
//!   which [`stack`] prints with the binary's UUID.
//!
//! macOS only; elsewhere both answer `None` and the watchdog keeps its
//! heartbeat rule alone.

use std::time::Duration;

/// Remember the calling thread as the one to probe. Called once, on the main
/// thread, by `watchdog::start`.
pub(crate) fn register() {
    #[cfg(target_os = "macos")]
    imp::register();
}

/// The registered thread's accumulated CPU time (user + system).
pub(crate) fn cpu_time() -> Option<Duration> {
    #[cfg(target_os = "macos")]
    return imp::cpu_time();
    #[cfg(not(target_os = "macos"))]
    None
}

/// The registered thread's stack, symbolized, one frame per entry, preceded by
/// the main executable's load address and UUID; `None` when it cannot be read.
pub(crate) fn stack() -> Option<Vec<String>> {
    #[cfg(target_os = "macos")]
    return imp::stack();
    #[cfg(not(target_os = "macos"))]
    None
}

/// Deepest stack the capture walks. A run-loop spin sits ~20 frames down; the
/// cap bounds the time the main thread stays suspended.
const MAX_FRAMES: usize = 64;

/// The frame-walk decision, over a memory reader, so it is testable on a
/// synthetic stack: frame records are `[caller fp, return address]` pairs that
/// must climb strictly upward; the walk stops at a null, misaligned or
/// non-increasing frame pointer, an unreadable record, or a zero return
/// address. `strip` removes pointer-authentication bits.
fn walk(
    pc: u64,
    fp: u64,
    mut read: impl FnMut(u64) -> Option<[u64; 2]>,
    strip: impl Fn(u64) -> u64,
    out: &mut [u64; MAX_FRAMES],
) -> usize {
    let mut n = 0;
    out[n] = strip(pc);
    n += 1;
    let mut fp = fp;
    while n < MAX_FRAMES {
        if fp == 0 || !fp.is_multiple_of(8) {
            break;
        }
        let Some([next_fp, ret]) = read(fp) else {
            break;
        };
        let ret = strip(ret);
        if ret == 0 {
            break;
        }
        out[n] = ret;
        n += 1;
        if next_fp <= fp {
            break;
        }
        fp = next_fp;
    }
    n
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, c_char, c_void};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::{MAX_FRAMES, walk};

    type MachPort = u32;
    type KernReturn = i32;

    #[repr(C)]
    struct DlInfo {
        fname: *const c_char,
        fbase: *mut c_void,
        sname: *const c_char,
        saddr: *mut c_void,
    }

    unsafe extern "C" {
        static mach_task_self_: MachPort;
        fn pthread_self() -> *mut c_void;
        fn pthread_mach_thread_np(thread: *mut c_void) -> MachPort;
        fn thread_suspend(thread: MachPort) -> KernReturn;
        fn thread_resume(thread: MachPort) -> KernReturn;
        fn thread_get_state(
            thread: MachPort,
            flavor: i32,
            state: *mut u32,
            count: *mut u32,
        ) -> KernReturn;
        fn thread_info(
            thread: MachPort,
            flavor: u32,
            info: *mut i32,
            count: *mut u32,
        ) -> KernReturn;
        fn vm_read_overwrite(
            task: MachPort,
            address: usize,
            size: usize,
            data: usize,
            out_size: *mut usize,
        ) -> KernReturn;
        fn dladdr(addr: *const c_void, info: *mut DlInfo) -> i32;
    }

    /// The main thread's port; 0 until registered. `pthread_mach_thread_np`
    /// hands out the thread's own name, no extra right to release.
    static MAIN: AtomicU32 = AtomicU32::new(0);

    pub(super) fn register() {
        // SAFETY: the calling thread's own pthread handle and port name.
        let port = unsafe { pthread_mach_thread_np(pthread_self()) };
        MAIN.store(port, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn unregister() {
        MAIN.store(0, Ordering::Relaxed);
    }

    fn main_port() -> Option<MachPort> {
        let port = MAIN.load(Ordering::Relaxed);
        (port != 0).then_some(port)
    }

    const THREAD_BASIC_INFO: u32 = 3;
    const THREAD_BASIC_INFO_COUNT: u32 = 10;

    pub(super) fn cpu_time() -> Option<Duration> {
        let port = main_port()?;
        // `thread_basic_info`: user_time {sec, usec}, system_time {sec, usec},
        // then six `integer_t` fields this reads nothing from.
        let mut info = [0i32; THREAD_BASIC_INFO_COUNT as usize];
        let mut count = THREAD_BASIC_INFO_COUNT;
        // SAFETY: `info` holds `count` integers, the size the flavor needs.
        let kr = unsafe { thread_info(port, THREAD_BASIC_INFO, info.as_mut_ptr(), &raw mut count) };
        if kr != 0 {
            return None;
        }
        let part = |sec: i32, usec: i32| {
            Duration::from_secs(u64::try_from(sec).unwrap_or(0))
                + Duration::from_micros(u64::try_from(usec).unwrap_or(0))
        };
        Some(part(info[0], info[1]) + part(info[2], info[3]))
    }

    /// `(pc, fp)` from a thread's saved registers.
    ///
    /// # Safety
    /// `port` names a thread of this task that is SUSPENDED.
    #[cfg(target_arch = "aarch64")]
    unsafe fn pc_fp(port: MachPort) -> Option<(u64, u64)> {
        const ARM_THREAD_STATE64: i32 = 6;
        // x0..x28, fp, lr, sp, pc, then cpsr + flags in the last u64.
        let mut state = [0u64; 34];
        let mut count = 68u32;
        // SAFETY: `state` is 68 u32 words, the flavor's size.
        let kr = unsafe {
            thread_get_state(
                port,
                ARM_THREAD_STATE64,
                state.as_mut_ptr().cast::<u32>(),
                &raw mut count,
            )
        };
        (kr == 0).then(|| (state[32], state[29]))
    }

    /// # Safety
    /// As the aarch64 twin.
    #[cfg(target_arch = "x86_64")]
    unsafe fn pc_fp(port: MachPort) -> Option<(u64, u64)> {
        const X86_THREAD_STATE64: i32 = 4;
        // rax rbx rcx rdx rdi rsi rbp rsp r8..r15 rip rflags cs fs gs.
        let mut state = [0u64; 21];
        let mut count = 42u32;
        // SAFETY: `state` is 42 u32 words, the flavor's size.
        let kr = unsafe {
            thread_get_state(
                port,
                X86_THREAD_STATE64,
                state.as_mut_ptr().cast::<u32>(),
                &raw mut count,
            )
        };
        (kr == 0).then(|| (state[16], state[6]))
    }

    /// Strip pointer-authentication bits: user addresses on macOS fit in 47.
    fn strip(addr: u64) -> u64 {
        addr & 0x0000_7FFF_FFFF_FFFF
    }

    /// One frame record, read by the kernel so a bad pointer is an error, not
    /// a fault.
    fn read_record(fp: u64) -> Option<[u64; 2]> {
        let mut record = [0u64; 2];
        let mut got = 0usize;
        // SAFETY: the kernel copies 16 bytes of this task's memory into
        // `record` (16 bytes) or fails without touching it.
        let kr = unsafe {
            vm_read_overwrite(
                mach_task_self_,
                usize::try_from(fp).ok()?,
                16,
                record.as_mut_ptr() as usize,
                &raw mut got,
            )
        };
        (kr == 0 && got == 16).then_some(record)
    }

    /// Suspend, read, resume. Nothing between the suspend and the resume
    /// allocates, locks or can panic: Mach calls into a stack array only.
    fn capture(port: MachPort, out: &mut [u64; MAX_FRAMES]) -> usize {
        // SAFETY: `port` is a thread of this task; it is resumed below on every
        // path that suspended it.
        if unsafe { thread_suspend(port) } != 0 {
            return 0;
        }
        // SAFETY: suspended just above.
        let n = match unsafe { pc_fp(port) } {
            Some((pc, fp)) => walk(pc, fp, read_record, strip, out),
            None => 0,
        };
        // SAFETY: balances the successful suspend.
        unsafe { thread_resume(port) };
        n
    }

    fn cstr(p: *const c_char) -> Option<String> {
        // SAFETY: `dladdr` returns NUL-terminated strings owned by the loader,
        // or null.
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }

    /// The main executable's `(base, UUID)`, read from its Mach-O header.
    fn exe_identity() -> Option<(usize, String)> {
        let mut info = DlInfo {
            fname: std::ptr::null(),
            fbase: std::ptr::null_mut(),
            sname: std::ptr::null(),
            saddr: std::ptr::null_mut(),
        };
        // SAFETY: `register` is code in this executable; `info` is live.
        if unsafe { dladdr(register as *const c_void, &raw mut info) } == 0 || info.fbase.is_null()
        {
            return None;
        }
        let base = info.fbase as usize;
        // mach_header_64: magic, cputype, cpusubtype, filetype, ncmds,
        // sizeofcmds, flags, reserved — then the load commands.
        // SAFETY: `base` is the mapped header of this executable.
        let ncmds = unsafe { std::ptr::read_unaligned((base + 16) as *const u32) };
        let mut cmd_at = base + 32;
        for _ in 0..ncmds.min(512) {
            // SAFETY: load commands follow the header inside the mapping.
            let (cmd, size) = unsafe {
                (
                    std::ptr::read_unaligned(cmd_at as *const u32),
                    std::ptr::read_unaligned((cmd_at + 4) as *const u32),
                )
            };
            const LC_UUID: u32 = 0x1b;
            if cmd == LC_UUID {
                // SAFETY: an LC_UUID command carries 16 bytes after its header.
                let bytes = unsafe { std::ptr::read_unaligned((cmd_at + 8) as *const [u8; 16]) };
                let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
                let uuid = format!(
                    "{}-{}-{}-{}-{}",
                    &hex[0..8],
                    &hex[8..12],
                    &hex[12..16],
                    &hex[16..20],
                    &hex[20..32]
                );
                return Some((base, uuid));
            }
            if size < 8 {
                break;
            }
            cmd_at += size as usize;
        }
        Some((base, "unknown".into()))
    }

    fn symbolize(pc: u64, exe_base: Option<usize>) -> String {
        let mut info = DlInfo {
            fname: std::ptr::null(),
            fbase: std::ptr::null_mut(),
            sname: std::ptr::null(),
            saddr: std::ptr::null_mut(),
        };
        // SAFETY: `dladdr` only looks the address up; `info` is live.
        let found = unsafe { dladdr(pc as *const c_void, &raw mut info) } != 0;
        if !found || info.fbase.is_null() {
            return format!("{pc:#x}");
        }
        let image = cstr(info.fname)
            .map(|p| p.rsplit('/').next().unwrap_or(&p).to_string())
            .unwrap_or_else(|| "?".into());
        let offset = pc as usize - info.fbase as usize;
        let own = exe_base == Some(info.fbase as usize);
        // A stripped release keeps almost no symbols, so the "nearest" one
        // dladdr finds in it is a lie; its frames are `aterm + offset`, for atos.
        match cstr(info.sname) {
            Some(sym) if !own && !info.saddr.is_null() && info.saddr != info.fbase => {
                format!(
                    "{image} + {offset} ({sym} + {})",
                    pc as usize - info.saddr as usize
                )
            }
            _ => format!("{image} + {offset}"),
        }
    }

    pub(super) fn stack() -> Option<Vec<String>> {
        let port = main_port()?;
        let mut pcs = [0u64; MAX_FRAMES];
        let n = capture(port, &mut pcs);
        if n == 0 {
            return None;
        }
        let exe = exe_identity();
        let mut lines = Vec::with_capacity(n + 1);
        if let Some((base, uuid)) = &exe {
            lines.push(format!("load address {base:#x}, UUID {uuid}"));
        }
        let exe_base = exe.map(|(base, _)| base);
        lines.extend(
            pcs[..n]
                .iter()
                .enumerate()
                .map(|(i, &pc)| format!("#{i} {}", symbolize(pc, exe_base))),
        );
        Some(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic stack: three records climbing upward, then a null caller.
    #[test]
    fn the_walk_follows_frame_records_upward_and_stops_cleanly() {
        let mem = |fp: u64| match fp {
            0x1000 => Some([0x1100, 0xA1]),
            0x1100 => Some([0x1200, 0xA2]),
            0x1200 => Some([0, 0xA3]),
            _ => None,
        };
        let mut out = [0u64; MAX_FRAMES];
        let n = walk(0xA0, 0x1000, mem, |a| a, &mut out);
        assert_eq!(&out[..n], &[0xA0, 0xA1, 0xA2, 0xA3]);
        // A loop (a record pointing at itself or below) stops the walk.
        let looped = |_fp: u64| Some([0x1000, 0xB1]);
        let n = walk(0xB0, 0x1000, looped, |a| a, &mut out);
        assert_eq!(&out[..n], &[0xB0, 0xB1]);
        // A misaligned or unreadable frame pointer yields the pc alone.
        assert_eq!(walk(0xC0, 0x1003, mem, |a| a, &mut out), 1);
        assert_eq!(walk(0xC0, 0x9000, mem, |a| a, &mut out), 1);
        // Authentication bits are stripped from every address.
        let signed = |fp: u64| (fp == 0x1000).then_some([0, 0x00AB_0000_0000_00A1]);
        let n = walk(
            0x0012_0000_0000_00A0,
            0x1000,
            signed,
            |a| a & 0xFFFF,
            &mut out,
        );
        assert_eq!(&out[..n], &[0xA0, 0xA1]);
    }

    /// The live probe against a real spinning thread (a spawned stand-in for
    /// the main thread, registered the way `watchdog::start` registers the
    /// real one): its CPU time advances while it spins, and suspending it
    /// yields a frame chain down to pthread's thread entry.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_probe_reads_a_spinning_threads_cpu_and_stack() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let stop = Arc::new(AtomicBool::new(false));
        let registered = Arc::new(AtomicBool::new(false));
        let (s, r) = (stop.clone(), registered.clone());
        let spinner = std::thread::spawn(move || {
            register();
            r.store(true, Ordering::SeqCst);
            let mut x = 0u64;
            while !s.load(Ordering::Relaxed) {
                x = std::hint::black_box(x.wrapping_add(1));
            }
            x
        });
        while !registered.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        // CPU time is read until it has visibly advanced, not over a fixed
        // wall window: under a loaded parallel suite the spinner may get a
        // fraction of a core, and the claim is only that the reading moves.
        let before = cpu_time().expect("thread_info on the registered thread");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut used = Duration::ZERO;
        while used < Duration::from_millis(50) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            used = cpu_time().expect("thread_info") - before;
        }
        let lines = stack().expect("a stack for the registered thread");
        stop.store(true, Ordering::Relaxed);
        let _ = spinner.join();
        // The spinner's port name dies with it; nothing may probe it again.
        imp::unregister();
        assert!(
            used >= Duration::from_millis(50),
            "a spinning thread's CPU time advances: {used:?}"
        );
        assert!(lines[0].starts_with("load address 0x"), "{lines:?}");
        assert!(
            lines.len() >= 3,
            "a frame chain, not just the pc: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("libsystem_pthread.dylib")),
            "the spawned thread's root frames are pthread's: {lines:?}"
        );
    }
}
