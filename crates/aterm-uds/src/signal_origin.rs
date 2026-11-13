// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signal provenance: the handler writes a fixed packet; a dedicated reader resolves it.
//! SIGKILL cannot be caught. A reaped child's status establishes its signal, not its sender.

use std::io::{self, Read, Write as _};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

static WRITE: AtomicI32 = AtomicI32::new(-1);
static SENT: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
static LOST: AtomicUsize = AtomicUsize::new(0);
static RECORDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SIGNAL_ORIGIN_START: OnceLock<Result<(), String>> = OnceLock::new();
type Observer = Box<dyn Fn(&str) + Send + Sync>;
static OBSERVER: Mutex<Option<Observer>> = Mutex::new(None);

/// Attach the run's durable event and log sink. Called outside a signal handler.
pub fn observe(observer: impl Fn(&str) + Send + Sync + 'static) {
    *OBSERVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(observer));
}

/// Start before installing a handler. Failure is explicit; a full pipe never blocks a handler.
/// # Errors
/// The pipe, its nonblocking mode, or the resolver thread could not be created.
pub fn start() -> Result<(), String> {
    SIGNAL_ORIGIN_START.get_or_init(|| {
        let (mut read, write) = crate::pipe().map_err(|e| e.to_string())?;
        // SAFETY: fcntl changes only this owned pipe's status flags.
        if unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        std::thread::Builder::new().name("signal-origin".into()).spawn(move || {
            let mut bytes = [0u8; 16];
            while read.read_exact(&mut bytes).is_ok() {
                let word = |i| i32::from_ne_bytes(bytes[i..i + 4].try_into().unwrap());
                let (signal, pid, uid, code) = (word(0), word(4), word(8), word(12));
                let record = format!("signal={signal} sender_pid={pid} sender_uid={} si_code={code} receiver_pid={} {}", uid as u32, std::process::id(), identity(pid));
                emit(record);
                DONE.fetch_add(1, Ordering::Release);
            }
        }).map_err(|e| e.to_string())?;
        WRITE.store(write.into_raw_fd(), Ordering::Release);
        Ok(())
    }).clone()
}

/// Capture the kernel's packet without allocation, locks, process inspection or formatting.
/// # Safety
/// `info` must be the live siginfo pointer supplied to an SA_SIGINFO handler.
pub unsafe fn capture(signal: i32, info: *const libc::siginfo_t) {
    if info.is_null() {
        return;
    }
    // SAFETY: errno is thread-local; preserve the interrupted code's value across write.
    #[cfg(target_vendor = "apple")]
    let errno = unsafe { libc::__error() };
    #[cfg(not(target_vendor = "apple"))]
    let errno = unsafe { libc::__errno_location() };
    // SAFETY: the platform returns this thread's live errno pointer.
    let saved_errno = unsafe { *errno };
    // SAFETY: the caller supplies the kernel-owned packet for this handler invocation.
    let info = unsafe { &*info };
    #[cfg(target_vendor = "apple")]
    let (pid, uid) = (info.si_pid, info.si_uid);
    #[cfg(not(target_vendor = "apple"))]
    // SAFETY: these fields are present in user-generated signal packets.
    let (pid, uid) = unsafe { (info.si_pid(), info.si_uid()) };
    let words = [signal, pid, uid as i32, info.si_code];
    let fd = WRITE.load(Ordering::Acquire);
    SENT.fetch_add(1, Ordering::Release);
    // SAFETY: write is async-signal-safe. The packet fits PIPE_BUF and the pipe is nonblocking.
    if fd < 0 || unsafe { libc::write(fd, words.as_ptr().cast(), 16) } != 16 {
        LOST.fetch_add(1, Ordering::Relaxed);
        DONE.fetch_add(1, Ordering::Release);
    }
    // SAFETY: this is the same thread-local pointer read above.
    unsafe { *errno = saved_errno };
}

fn emit(record: String) {
    let _ = writeln!(std::io::stderr().lock(), "{record}");
    if let Some(observer) = &*OBSERVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        observer(&record);
    }
    RECORDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(record);
}

/// Record a wait status outside a handler. A wait status cannot establish the sender.
pub(crate) fn child_death(pid: i32, signal: i32) {
    if WRITE.load(Ordering::Acquire) >= 0 {
        emit(format!(
            "child-exit child_pid={pid} signal={signal} parent_pid={} sender=unavailable-from-wait-status",
            std::process::id()
        ));
    }
}

/// Resolve a live process now, accepting path and argv only between matching birth reads.
pub fn identity(pid: i32) -> String {
    let before = crate::process::read_process_start(pid);
    let argv = crate::process::read_process_argv(pid);
    #[cfg(target_vendor = "apple")]
    let path = crate::process::read_process_path(pid);
    #[cfg(target_os = "linux")]
    let path = u32::try_from(pid)
        .ok()
        .and_then(crate::process::read_exec_path);
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    let path: Option<std::path::PathBuf> = None;
    if before.is_none() || before != crate::process::read_process_start(pid) {
        return "executable=unavailable argv=unavailable (process exited or identity changed)"
            .into();
    }
    format!("executable={path:?} argv={argv:?}")
}

/// All packets resolved so far, waiting at most one second for a packet already captured.
pub fn records() -> Vec<String> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while DONE.load(Ordering::Acquire) < SENT.load(Ordering::Acquire)
        && std::time::Instant::now() < until
    {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let mut records = RECORDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let lost = LOST.load(Ordering::Relaxed);
    if lost != 0 {
        records.push(format!("signal provenance lost_packets={lost}"));
    }
    records
}

#[cfg(test)]
#[path = "signal_origin_tests.rs"]
mod tests;
