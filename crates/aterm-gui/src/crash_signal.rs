// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Async-signal-safe capture of native fatal signals (M6 "CRASH-CORE").
//!
//! The panic hook in [`crate::logging`] only fires for Rust unwinds. Native
//! fatal signals — `SIGSEGV`, `SIGABRT`, `SIGBUS`, `SIGILL`, `SIGFPE` — never
//! run the panic machinery: they jump straight to the kernel-installed
//! disposition and the process is gone, with nothing on disk for a windowed
//! app whose stderr nobody reads. This module installs a `sigaction`(2)
//! handler that drops a crash *marker* before the process dies, so an
//! after-the-fact "did aterm take a signal, and which one?" question has an
//! artifact to point at.
//!
//! ASYNC-SIGNAL-SAFETY (the whole reason this is its own module):
//! a signal handler may interrupt the program at *any* instruction, including
//! the middle of `malloc`, a `Mutex` critical section, or libc's own buffers.
//! It may therefore call ONLY async-signal-safe functions (POSIX
//! `signal-safety(7)`): here that is `write(2)`, `sigaction(2)`, and
//! `raise(3)`. Everything that is NOT signal-safe — resolving the marker path,
//! `open`ing it, allocating, `format!` — is done ONCE AT INSTALL TIME and the
//! results are parked in `static`s. The handler itself:
//!   * does no allocation (the integer is formatted into a stack buffer with a
//!     hand-rolled, allocation-free decimal helper),
//!   * takes no lock,
//!   * calls no `format!`/`String`/`println!`,
//!   * writes a pre-built banner + the signal number to `STDERR_FILENO` and to
//!     a pre-opened marker fd, then
//!   * restores the signal's default disposition and `raise()`s it, so the
//!     process still core-dumps / exits with the conventional status.

/// Arm async-signal-safe fatal-signal capture (`SIGSEGV`/`SIGABRT`/`SIGBUS`/
/// `SIGILL`/`SIGFPE`). Call alongside the panic hook so both crash paths are
/// covered. On Windows the analogue is an unhandled-exception filter
/// (`SetUnhandledExceptionFilter`) writing a crash artifact under the same
/// discipline (everything non-trivial at install time; the filter itself
/// allocates nothing). No-op on targets with neither lane so the call site
/// stays portable.
///
/// COVERAGE IS NOT SYMMETRIC across platforms, and deliberately so. The unix
/// lane traps `SIGABRT`, which is where Rust's abort paths land — allocation
/// failure (`handle_alloc_error`), a panic-while-panicking, and
/// `std::process::abort` all funnel to `abort()` → `SIGABRT`, so those get a
/// marker. On Windows, Rust's `abort_internal` is `int 0x29` (`__fastfail`
/// with `FAST_FAIL_FATAL_APP_EXIT`), which terminates the process WITHOUT
/// running any in-process exception handler — the SUEF filter (and VEH) never
/// see it, so an OOM abort or double panic leaves no marker from *this* lane.
/// The double-panic case is still partly covered: the panic hook already wrote
/// `crash-<pid>.log` for the first panic. The only way to capture a bare
/// `__fastfail` on Windows is out-of-process, via WER LocalDumps
/// (`HKCU\Software\Microsoft\Windows\Windows Error Reporting\LocalDumps\<exe>`),
/// which is an install-time/deployment concern, not something this lane can arm.
pub fn install_signal_handlers() {
    #[cfg(unix)]
    imp::install_signal_handlers();
    #[cfg(windows)]
    imp_windows::install();
}

/// Launch-unique marker path: `crash-signal-<pid>-<nanos>.log`. Windows recycles
/// PIDs aggressively, so keying on PID alone lets a later launch that inherits a
/// crashed instance's PID truncate that instance's genuine marker at startup.
/// Salting with the startup timestamp keeps a recycled PID from clobbering a
/// prior crash's artifact. Called AT INSTALL TIME — allocation is fine here.
#[cfg(any(unix, windows))]
fn marker_path(dir: &std::path::Path) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("crash-signal-{}-{}.log", std::process::id(), nanos))
}

/// Delete zero-length `crash-signal-*.log` files left in `dir`. Every clean run
/// creates an empty marker and never writes to it, so without this sweep they
/// accumulate one stale file per launch, unbounded. Only *empty* markers are
/// removed — a non-empty one is a real crash record we must preserve. Called AT
/// INSTALL TIME (before our own marker is created), where `read_dir`/allocation
/// are fine; a concurrently-running instance's still-empty marker may be swept,
/// which is harmless (it had recorded no crash).
#[cfg(any(unix, windows))]
fn sweep_stale_markers(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("crash-signal-")
            && name.ends_with(".log")
            && entry.metadata().map(|m| m.len() == 0).unwrap_or(false)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    /// Fatal signals we trap. Each bypasses the Rust panic hook entirely, so
    /// without this handler they leave no on-disk trace for a windowed app.
    const FATAL_SIGNALS: [i32; 5] = [
        libc::SIGSEGV,
        libc::SIGABRT,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
    ];

    /// Static, pre-built banner halves written around the formatted signal
    /// number. Keeping these as `&[u8]` constants means the handler does no
    /// string work at signal time — it just `write(2)`s known bytes. The
    /// suffix's leading byte is a space; `\u{2014}` is the em dash (UTF-8
    /// `E2 80 94`).
    const BANNER_PREFIX: &[u8] = b"aterm: fatal signal ";
    const BANNER_SUFFIX: &[u8] = " \u{2014} crash marker written\n".as_bytes();

    /// fd of the pre-opened crash-marker file, or `-1` when none was opened
    /// (no writable private dir at install time). `AtomicI32` so the handler
    /// reads it without a lock; written once during `install`.
    static MARKER_FD: AtomicI32 = AtomicI32::new(-1);

    /// Tripped once we have armed the `sigaction` handlers, so a second
    /// `install` call (defensive) does not re-open the marker fd.
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// Maximum decimal digits a `u32` ever needs (`4294967295`). A signal
    /// number is far smaller, but sizing for `u32` keeps the helper reusable
    /// and the buffer trivially large enough.
    const U32_MAX_DIGITS: usize = 10;

    /// Format `value` as decimal ASCII into the tail of `buf`, returning the
    /// filled sub-slice. ALLOCATION-FREE and async-signal-safe: it only writes
    /// bytes into the caller's stack buffer, so it is callable from the signal
    /// handler. Digits are produced least-significant-first from the end of the
    /// buffer, then the populated tail slice is returned. Zero yields `"0"`.
    fn fmt_u32_decimal(value: u32, buf: &mut [u8; U32_MAX_DIGITS]) -> &[u8] {
        let mut n = value;
        // Index just past the last byte; we fill backwards from here.
        let mut pos = buf.len();
        loop {
            pos -= 1;
            buf[pos] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        &buf[pos..]
    }

    /// Async-signal-safe `write(2)` of `bytes` to `fd`, looping over short and
    /// `EINTR`-interrupted writes. Returns once everything is written or a
    /// non-retryable error is seen. Errors are swallowed: a crash handler that
    /// cannot write its marker must still fall through to re-raising the signal.
    fn write_all_signal_safe(fd: i32, bytes: &[u8]) {
        if fd < 0 {
            return;
        }
        let mut off = 0usize;
        while off < bytes.len() {
            // SAFETY: `bytes[off..]` is a live slice for the duration of the
            // call; `fd` is checked non-negative above. `write` is on the POSIX
            // async-signal-safe list, so calling it from a signal handler is
            // sound. A short (`n >= 0`) or error (`n < 0`) return is handled
            // below; we never deref the return as a pointer.
            let n = unsafe {
                libc::write(
                    fd,
                    bytes[off..].as_ptr().cast::<libc::c_void>(),
                    bytes.len() - off,
                )
            };
            if n > 0 {
                off += n as usize;
                continue;
            }
            // n == 0 (nothing written) or n < 0 (error). Retry only on EINTR;
            // otherwise give up so we still re-raise the signal. `errno` is a
            // thread-local read, which is async-signal-safe.
            if n < 0 {
                // SAFETY: `errno_location()` returns a valid, thread-local
                // `*mut c_int`; reading it on this thread is sound and
                // async-signal-safe.
                let err = unsafe { *errno_location() };
                if err == libc::EINTR {
                    continue;
                }
            }
            break;
        }
    }

    /// Pointer to the calling thread's `errno` (`__error` on macOS/BSD,
    /// `__errno_location` elsewhere). Both are async-signal-safe.
    ///
    /// # Safety
    /// The returned pointer is valid for the calling thread only; deref it on
    /// that thread.
    #[inline]
    unsafe fn errno_location() -> *mut libc::c_int {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: `__error` returns this thread's errno address.
            unsafe { libc::__error() }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // SAFETY: `__errno_location` returns this thread's errno address.
            unsafe { libc::__errno_location() }
        }
    }

    /// The fatal-signal handler. Installed without `SA_SIGINFO`, so it receives
    /// only the signal number. EVERYTHING here is async-signal-safe: a stack
    /// buffer, the allocation-free decimal helper, `write(2)`, `sigaction(2)`
    /// to restore the default disposition, and `raise(3)`.
    extern "C" fn handle_fatal_signal(sig: libc::c_int) {
        // Format the (non-negative) signal number with no allocation.
        let mut digits = [0u8; U32_MAX_DIGITS];
        let num = fmt_u32_decimal(sig.max(0) as u32, &mut digits);

        // Banner -> STDERR, then the same banner -> the pre-opened marker fd.
        let marker_fd = MARKER_FD.load(Ordering::Relaxed);
        for fd in [libc::STDERR_FILENO, marker_fd] {
            write_all_signal_safe(fd, BANNER_PREFIX);
            write_all_signal_safe(fd, num);
            write_all_signal_safe(fd, BANNER_SUFFIX);
        }

        // Restore the default disposition for THIS signal and re-raise it, so
        // the process dies the conventional way (core dump / exit status),
        // exactly as if we had never trapped it.
        //
        // SAFETY: `act` is a fully-zeroed, correctly-sized `sigaction` with
        // `sa_sigaction = SIG_DFL`; `sigaction`/`raise` are async-signal-safe.
        // We ignore the return values: if restoring fails we still `raise`,
        // and a handler must not unwind, so there is nothing to propagate.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &act, std::ptr::null_mut());
            libc::raise(sig);
        }
    }

    /// Open a launch-unique crash-marker file `0600` and return its raw fd, or
    /// `-1` when no private dir is available. Sweeps stale empty markers first.
    /// Done AT INSTALL TIME — `open`/path work is not signal-safe.
    fn open_marker_fd() -> i32 {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::IntoRawFd;
        let Some(dir) = crate::logging::log_dir() else {
            return -1;
        };
        super::sweep_stale_markers(&dir);
        let path = super::marker_path(&dir);
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true).mode(0o600);
        match opts.open(&path) {
            // Leak the `File` into a raw fd: the marker must outlive this
            // function and stay open for the whole process so the handler can
            // write to it. The OS reclaims it at exit.
            Ok(f) => f.into_raw_fd(),
            Err(_) => -1,
        }
    }

    /// Arm async-signal-safe capture of the fatal signals. Idempotent: the
    /// marker fd is opened (and handlers installed) only on the first call.
    pub fn install_signal_handlers() {
        if ARMED.swap(true, Ordering::SeqCst) {
            return; // already armed
        }
        // Pre-open the marker fd now (NOT in the handler — `open` is unsafe in
        // a signal context). A `-1` simply means the handler writes to stderr
        // only.
        MARKER_FD.store(open_marker_fd(), Ordering::SeqCst);

        for &sig in &FATAL_SIGNALS {
            // SAFETY: `act` is a zeroed `sigaction` with a valid function
            // pointer in `sa_sigaction`, an empty (zeroed, then `sigemptyset`)
            // signal mask, and standard flags. `sigaction` is called outside
            // any signal context (install time) with a correctly-shaped struct,
            // so the call is sound. We ignore the result — a failure to arm one
            // signal must not abort startup of the terminal.
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                // The `sa_sigaction` field is the function-pointer slot; libc
                // types it as `usize`, so cast via a thin pointer (not a direct
                // fn-item-to-int cast, which lints).
                act.sa_sigaction = handle_fatal_signal as *const () as usize;
                // SA_RESTART so syscalls we trap *out of* are restarted where
                // possible. SA_NODEFER is intentionally NOT set, so the signal
                // stays masked while our handler runs.
                act.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut act.sa_mask);
                libc::sigaction(sig, &act, std::ptr::null_mut());
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Render `value` via the allocation-free helper into a `String` for
        /// easy comparison against `to_string`.
        fn render(value: u32) -> String {
            let mut buf = [0u8; U32_MAX_DIGITS];
            let bytes = fmt_u32_decimal(value, &mut buf);
            String::from_utf8(bytes.to_vec()).unwrap()
        }
        // (imp_windows carries the mirror-image tests for its hex helper.)

        #[test]
        fn decimal_formatter_matches_std_for_many_values() {
            // Exhaustive small range + boundaries + the widest u32 cases.
            for v in 0u32..2000 {
                assert_eq!(render(v), v.to_string(), "mismatch at {v}");
            }
            for v in [
                0,
                1,
                9,
                10,
                11,
                99,
                100,
                255, // max u8 — bigger than any signal number
                999,
                1000,
                65_535,
                1_000_000,
                4_294_967_294,
                u32::MAX, // 4294967295 — the widest 10-digit case
            ] {
                assert_eq!(render(v), v.to_string(), "mismatch at {v}");
            }
        }

        #[test]
        fn decimal_formatter_handles_zero_without_dropping_the_digit() {
            assert_eq!(render(0), "0");
        }

        #[test]
        fn every_fatal_signal_number_formats_within_the_buffer() {
            for &sig in &FATAL_SIGNALS {
                let mut buf = [0u8; U32_MAX_DIGITS];
                let bytes = fmt_u32_decimal(sig as u32, &mut buf);
                assert_eq!(
                    bytes,
                    sig.to_string().as_bytes(),
                    "signal {sig} formatted wrong"
                );
            }
        }

        #[test]
        fn banner_bytes_are_the_expected_marker_text() {
            assert_eq!(BANNER_PREFIX, b"aterm: fatal signal ");
            // Suffix is a leading space + em dash (U+2014, UTF-8 E2 80 94) +
            // text + newline.
            assert_eq!(
                BANNER_SUFFIX,
                &[
                    b' ', 0xE2, 0x80, 0x94, b' ', b'c', b'r', b'a', b's', b'h', b' ', b'm', b'a',
                    b'r', b'k', b'e', b'r', b' ', b'w', b'r', b'i', b't', b't', b'e', b'n', b'\n',
                ]
            );
            // The assembled banner around a sample signal number reads
            // correctly end to end.
            let mut buf = [0u8; U32_MAX_DIGITS];
            let num = fmt_u32_decimal(libc::SIGSEGV as u32, &mut buf);
            let mut line = Vec::new();
            line.extend_from_slice(BANNER_PREFIX);
            line.extend_from_slice(num);
            line.extend_from_slice(BANNER_SUFFIX);
            let text = String::from_utf8(line).unwrap();
            assert_eq!(
                text,
                format!(
                    "aterm: fatal signal {} \u{2014} crash marker written\n",
                    libc::SIGSEGV
                )
            );
        }

        #[test]
        fn install_is_idempotent_and_arms_a_handler_for_sigsegv() {
            // Calling twice must not panic and must not re-open the marker.
            install_signal_handlers();
            install_signal_handlers();

            // Read back the current SIGSEGV disposition with a null `act`; a
            // handler should now be installed (neither SIG_DFL nor SIG_IGN).
            // We do NOT raise the signal.
            //
            // SAFETY: `old` is a zeroed, correctly-sized `sigaction`; passing a
            // null `act` makes `sigaction` a pure query of the current
            // disposition, which it writes into `old`. Called at test (non-
            // signal) time.
            let installed = unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                let rc = libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut old);
                rc == 0 && old.sa_sigaction != libc::SIG_DFL && old.sa_sigaction != libc::SIG_IGN
            };
            assert!(
                installed,
                "SIGSEGV should have a custom handler after install"
            );
        }
    }
}

/// The Windows crash lane: an unhandled-exception filter
/// (`SetUnhandledExceptionFilter`) writing a crash artifact. Native fatal
/// exceptions dispatched through the OS exception machinery — access violation
/// `0xC0000005`, illegal instruction, stack overflow, etc. — bypass the Rust
/// panic hook the way POSIX fatal signals do, and this filter catches them
/// under the unix module's discipline: everything non-trivial (path
/// resolution, `open`, allocation) happens ONCE at install time, and the
/// filter itself does no allocation (stack hex buffers + pre-built banner
/// bytes + `WriteFile` loops), because it may run with a corrupted heap.
///
/// It does NOT mirror the unix `SIGABRT` coverage. Rust's `abort_internal` on
/// Windows is `__fastfail` (`int 0x29`), which the kernel terminates on
/// *without* dispatching to any in-process filter — so OOM/`process::abort`/
/// double-panic never reach here. See [`install_signal_handlers`] for the full
/// asymmetry and the WER-LocalDumps escape hatch.
#[cfg(windows)]
mod imp_windows {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetUnhandledExceptionFilter(
            filter: extern "system" fn(*mut ExceptionPointers) -> i32,
        ) -> *mut core::ffi::c_void;
        fn GetStdHandle(which: u32) -> isize;
        fn WriteFile(
            handle: isize,
            data: *const u8,
            len: u32,
            written: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
    }

    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const INVALID_HANDLE_VALUE: isize = -1;
    /// Hand the exception on to the default WER/termination path after the
    /// marker is written — the analogue of the unix restore-default-and-raise.
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    /// Minimal `EXCEPTION_POINTERS`: only `ExceptionRecord` is followed.
    #[repr(C)]
    struct ExceptionPointers {
        exception_record: *mut ExceptionRecord,
        context_record: *mut core::ffi::c_void,
    }

    /// `EXCEPTION_RECORD` with its documented fixed layout. `#[repr(C)]` with
    /// the native field types reproduces the OS struct (including the
    /// alignment padding before `exception_information` on 64-bit), so we can
    /// read `ExceptionAddress` (the faulting PC) and, for an access violation,
    /// the read/write flag + faulting address from `ExceptionInformation` — all
    /// allocation-free, straight out of the record the filter is handed.
    #[repr(C)]
    struct ExceptionRecord {
        exception_code: u32,
        // Present only to place the fields after them at their native offsets;
        // we never read the flags or the nested-record pointer.
        #[allow(dead_code)]
        exception_flags: u32,
        #[allow(dead_code)]
        exception_record: *mut ExceptionRecord,
        exception_address: *mut core::ffi::c_void,
        number_parameters: u32,
        exception_information: [usize; 15],
    }

    /// `STATUS_ACCESS_VIOLATION`: the one code whose `ExceptionInformation`
    /// layout (op at [0], faulting address at [1]) we decode.
    const EXCEPTION_ACCESS_VIOLATION: u32 = 0xC000_0005;

    /// Static, pre-built banner fragments written around the hex fields, so the
    /// filter does no string work at exception time (same shape as the unix
    /// banner; `\u{2014}` is the em dash, UTF-8 `E2 80 94`). The assembled line
    /// is `PREFIX <code> AT <pc> [AV_OP <op> AV_AT <addr>] SUFFIX`.
    const BANNER_PREFIX: &[u8] = b"aterm: fatal exception 0x";
    const BANNER_AT: &[u8] = b" at 0x";
    const BANNER_AV_OP: &[u8] = " \u{2014} access violation ".as_bytes();
    const BANNER_AV_AT: &[u8] = b" @ 0x";
    const BANNER_SUFFIX: &[u8] = " \u{2014} crash marker written\n".as_bytes();

    /// Raw HANDLE of the pre-opened crash-marker file, or `-1` when none was
    /// opened (no writable log dir at install time). `AtomicIsize` so the
    /// filter reads it without a lock; written once during `install`.
    static MARKER_HANDLE: AtomicIsize = AtomicIsize::new(-1);

    /// Tripped once the filter is registered, so a second `install` call
    /// (defensive) does not re-open the marker handle.
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// Format `value` as exactly 8 uppercase hex digits into `buf`, returning
    /// the filled slice. ALLOCATION-FREE (the sibling of the unix module's
    /// `fmt_u32_decimal`), so it is callable from the exception filter.
    /// Fixed-width on purpose: NTSTATUS codes read conventionally as 8 digits
    /// (`0xC0000005`).
    fn fmt_u32_hex(value: u32, buf: &mut [u8; 8]) -> &[u8] {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for (i, b) in buf.iter_mut().enumerate() {
            *b = HEX[((value >> ((7 - i) * 4)) & 0xF) as usize];
        }
        &buf[..]
    }

    /// Format `value` as exactly 16 uppercase hex digits — the pointer-width
    /// sibling of [`fmt_u32_hex`], for `ExceptionAddress` and the faulting
    /// address. Also allocation-free and callable from the filter.
    fn fmt_u64_hex(value: u64, buf: &mut [u8; 16]) -> &[u8] {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for (i, b) in buf.iter_mut().enumerate() {
            *b = HEX[((value >> ((15 - i) * 4)) & 0xF) as usize];
        }
        &buf[..]
    }

    /// Allocation-free `WriteFile` loop over short writes — the filter-side
    /// analogue of the unix `write_all_signal_safe`. Errors are swallowed: a
    /// filter that cannot write its marker must still return so the default
    /// termination proceeds.
    fn write_all_handle(handle: isize, bytes: &[u8]) {
        if handle == INVALID_HANDLE_VALUE || handle == 0 {
            return;
        }
        let mut off = 0usize;
        while off < bytes.len() {
            let mut written = 0u32;
            let len = u32::try_from(bytes.len() - off).unwrap_or(u32::MAX);
            // SAFETY: `bytes[off..]` is a live slice for the call and `written`
            // is a valid out-param; `WriteFile` allocates nothing on our side,
            // so calling it with a possibly-corrupt heap is sound.
            let ok = unsafe {
                WriteFile(
                    handle,
                    bytes[off..].as_ptr(),
                    len,
                    &mut written,
                    core::ptr::null_mut(),
                )
            };
            if ok == 0 || written == 0 {
                break;
            }
            off += written as usize;
        }
    }

    /// The unhandled-exception filter. EVERYTHING here is allocation-free: a
    /// stack hex buffer, the pre-built banner bytes, and `WriteFile` loops to
    /// the stderr + marker handles. Returns `EXCEPTION_CONTINUE_SEARCH` so the
    /// default WER/termination path still runs.
    extern "system" fn handle_fatal_exception(info: *mut ExceptionPointers) -> i32 {
        let mut code = 0u32;
        let mut address = 0u64;
        let mut av: Option<(&[u8], u64)> = None;
        // SAFETY: the OS passes a valid `EXCEPTION_POINTERS`; both pointers are
        // guarded anyway (a null record just formats code 0). The record has
        // the documented fixed `#[repr(C)]` layout, so reading `number_parameters`
        // before indexing `exception_information` stays in bounds.
        unsafe {
            if !info.is_null() {
                let rec = (*info).exception_record;
                if !rec.is_null() {
                    code = (*rec).exception_code;
                    address = (*rec).exception_address as usize as u64;
                    if code == EXCEPTION_ACCESS_VIOLATION && (*rec).number_parameters >= 2 {
                        // ExceptionInformation[0]: 0 read, 1 write, 8 execute.
                        // ExceptionInformation[1]: the inaccessible address.
                        let op: &[u8] = match (*rec).exception_information[0] {
                            0 => b"reading",
                            1 => b"writing",
                            8 => b"executing",
                            _ => b"accessing",
                        };
                        av = Some((op, (*rec).exception_information[1] as u64));
                    }
                }
            }
        }
        let mut code_digits = [0u8; 8];
        let code_hex = fmt_u32_hex(code, &mut code_digits);
        let mut pc_digits = [0u8; 16];
        let pc_hex = fmt_u64_hex(address, &mut pc_digits);
        let mut av_digits = [0u8; 16];
        let av_hex = av.map(|(_, addr)| fmt_u64_hex(addr, &mut av_digits));

        // Banner -> stderr, then the same banner -> the pre-opened marker.
        // SAFETY: `GetStdHandle` is a plain handle query, callable anywhere.
        let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        let marker = MARKER_HANDLE.load(Ordering::Relaxed);
        for h in [stderr, marker] {
            write_all_handle(h, BANNER_PREFIX);
            write_all_handle(h, code_hex);
            write_all_handle(h, BANNER_AT);
            write_all_handle(h, pc_hex);
            if let (Some((op, _)), Some(av_hex)) = (av, av_hex) {
                write_all_handle(h, BANNER_AV_OP);
                write_all_handle(h, op);
                write_all_handle(h, BANNER_AV_AT);
                write_all_handle(h, av_hex);
            }
            write_all_handle(h, BANNER_SUFFIX);
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    /// Open a launch-unique crash-marker file and return its raw HANDLE, or `-1`
    /// when no log dir is available. Sweeps stale empty markers first. Done AT
    /// INSTALL TIME — path work and `open` must not happen inside the filter.
    fn open_marker_handle() -> isize {
        use std::os::windows::io::IntoRawHandle;
        let Some(dir) = crate::logging::log_dir() else {
            return -1;
        };
        super::sweep_stale_markers(&dir);
        let path = super::marker_path(&dir);
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
        {
            // Leak the `File` into a raw handle: the marker must stay open for
            // the whole process so the filter can write to it. The OS reclaims
            // it at exit.
            Ok(f) => f.into_raw_handle() as isize,
            Err(_) => -1,
        }
    }

    /// Arm the unhandled-exception filter. Idempotent: the marker handle is
    /// opened (and the filter registered) only on the first call.
    pub fn install() {
        if ARMED.swap(true, Ordering::SeqCst) {
            return; // already armed
        }
        MARKER_HANDLE.store(open_marker_handle(), Ordering::SeqCst);
        // SAFETY: registers a plain `extern "system"` function pointer with the
        // documented signature. The previous filter is deliberately dropped —
        // we are the outermost crash reporter, and we CONTINUE_SEARCH so the
        // default handling still runs after us.
        unsafe {
            SetUnhandledExceptionFilter(handle_fatal_exception);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Render `value` via the allocation-free hex helper (mirrors the unix
        /// module's decimal-helper tests).
        fn render(value: u32) -> String {
            let mut buf = [0u8; 8];
            String::from_utf8(fmt_u32_hex(value, &mut buf).to_vec()).unwrap()
        }

        #[test]
        fn hex_formatter_matches_std_for_many_values() {
            for v in 0u32..2000 {
                assert_eq!(render(v), format!("{v:08X}"), "mismatch at {v}");
            }
            for v in [
                0,
                1,
                0x8000_0003, // breakpoint
                0xC000_0005, // access violation — the classic
                0xC000_00FD, // stack overflow
                u32::MAX,
            ] {
                assert_eq!(render(v), format!("{v:08X}"), "mismatch at {v:#010X}");
            }
        }

        #[test]
        fn hex64_formatter_matches_std() {
            for v in [0u64, 1, 0xDEAD_BEEF, 0x7FF6_1234_5678_ABCD, u64::MAX] {
                let mut buf = [0u8; 16];
                assert_eq!(
                    String::from_utf8(fmt_u64_hex(v, &mut buf).to_vec()).unwrap(),
                    format!("{v:016X}"),
                    "mismatch at {v:#018X}"
                );
            }
        }

        #[test]
        fn banner_bytes_are_the_expected_marker_text() {
            // Non-AV assembly: code + faulting PC + suffix.
            let mut code_buf = [0u8; 8];
            let mut pc_buf = [0u8; 16];
            let code = fmt_u32_hex(0x8000_0003, &mut code_buf);
            let pc = fmt_u64_hex(0x7FF6_1234_5678_ABCD, &mut pc_buf);
            let mut line = Vec::new();
            line.extend_from_slice(BANNER_PREFIX);
            line.extend_from_slice(code);
            line.extend_from_slice(BANNER_AT);
            line.extend_from_slice(pc);
            line.extend_from_slice(BANNER_SUFFIX);
            assert_eq!(
                String::from_utf8(line).unwrap(),
                "aterm: fatal exception 0x80000003 at 0x7FF612345678ABCD \u{2014} crash marker written\n"
            );

            // Access-violation assembly threads the op + faulting address in.
            let av_pc = fmt_u64_hex(0x7FF6_0000_0100, &mut pc_buf);
            let mut av_buf = [0u8; 16];
            let av_addr = fmt_u64_hex(0, &mut av_buf);
            let mut code_buf = [0u8; 8];
            let av_code = fmt_u32_hex(0xC000_0005, &mut code_buf);
            let mut line = Vec::new();
            line.extend_from_slice(BANNER_PREFIX);
            line.extend_from_slice(av_code);
            line.extend_from_slice(BANNER_AT);
            line.extend_from_slice(av_pc);
            line.extend_from_slice(BANNER_AV_OP);
            line.extend_from_slice(b"writing");
            line.extend_from_slice(BANNER_AV_AT);
            line.extend_from_slice(av_addr);
            line.extend_from_slice(BANNER_SUFFIX);
            assert_eq!(
                String::from_utf8(line).unwrap(),
                "aterm: fatal exception 0xC0000005 at 0x00007FF600000100 \u{2014} access violation writing @ 0x0000000000000000 \u{2014} crash marker written\n"
            );
        }

        #[cfg(target_pointer_width = "64")]
        #[test]
        fn exception_record_layout_matches_native_offsets() {
            // On x64 the `#[repr(C)]` struct must reproduce the OS field
            // offsets, including the 4 bytes of alignment padding after
            // NumberParameters before ExceptionInformation.
            assert_eq!(core::mem::offset_of!(ExceptionRecord, exception_code), 0);
            assert_eq!(core::mem::offset_of!(ExceptionRecord, exception_flags), 4);
            assert_eq!(core::mem::offset_of!(ExceptionRecord, exception_record), 8);
            assert_eq!(
                core::mem::offset_of!(ExceptionRecord, exception_address),
                16
            );
            assert_eq!(
                core::mem::offset_of!(ExceptionRecord, number_parameters),
                24
            );
            assert_eq!(
                core::mem::offset_of!(ExceptionRecord, exception_information),
                32
            );
        }

        #[test]
        fn install_is_idempotent() {
            // Calling twice must not panic and must not re-open the marker.
            install();
            install();
            assert!(ARMED.load(Ordering::SeqCst));
        }
    }
}
