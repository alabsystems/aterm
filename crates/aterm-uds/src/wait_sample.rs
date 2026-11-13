// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit, identity-bound macOS wait sampling. Only a trusted system-library leaf can name
//! a wait; the raw report stays in the caller's private directory, never in the typed result.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::process::Identity;

/// What every sampled leaf agreed was waiting. This names no lock or lock holder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A timer's syscall.
    Timer,
    /// A file-lock syscall; ownership remains unknown.
    Lock,
    /// A plain read syscall.
    Read,
    /// An event wait.
    Event,
    /// A Mach message wait.
    Ipc,
    /// A mutex, condition-variable or unfair-lock wait.
    Mutex,
    /// A child wait.
    Children,
    /// No single, authenticated waiting leaf kind covered the report.
    Unparsed,
}

impl Kind {
    /// The closed, sanitized wire word.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Timer => "timer",
            Self::Lock => "lock",
            Self::Read => "read",
            Self::Event => "event",
            Self::Ipc => "ipc",
            Self::Mutex => "mutex",
            Self::Children => "children",
            Self::Unparsed => "unparsed",
        }
    }
}

/// One completed, identity-bracketed observation. It is not continuous ownership evidence.
#[derive(Debug)]
pub struct Sample {
    /// The common kind, or [`Kind::Unparsed`] on incomplete or conflicting stacks.
    pub kind: Kind,
    /// The retained private raw report, for the person to inspect.
    pub report: PathBuf,
    /// Time spent making and reading this observation, not a wait's age.
    pub elapsed: Duration,
}

/// The private raw report retained by a failed [`sample`] call, when collection got far enough
/// to reserve one. The error remains an [`io::Error`] with its original kind; this accessor is
/// the only way callers learn the untrusted report's path.
#[must_use]
pub fn retained_report(error: &io::Error) -> Option<&Path> {
    error
        .get_ref()?
        .downcast_ref::<RetainedReport>()
        .map(|retained| retained.report.as_path())
}

#[derive(Debug)]
struct RetainedReport {
    report: PathBuf,
    error: io::Error,
}

impl std::fmt::Display for RetainedReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for RetainedReport {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.get_ref().map(|error| error as _)
    }
}

#[cfg(any(target_os = "macos", test))]
fn retain_report(error: io::Error, report: &Path) -> io::Error {
    io::Error::new(
        error.kind(),
        RetainedReport {
            report: report.to_path_buf(),
            error,
        },
    )
}

/// Explicitly sample `expected` for one second, with 100 ms between stack readings. The
/// caller supplies an existing owner-private directory; this call never asks for permission,
/// escalates privileges, or automatically samples another pid. Same-UID birth and exec version
/// must match before and after the command. The sample tool has a one-minute hang detector.
/// `canceled` is checked before work and between waits of at most 25 ms. Cancellation sends
/// `SIGKILL` only to the owned sample child and waits at most 100 ms for its reap; a child the
/// kernel will not reap is left to the existing reaper. It never signals the sampled process.
/// Raw output is retained even when collection or classification fails. On success the private
/// report's path is in [`Sample`]; on failure [`retained_report`] returns it when collection got
/// far enough to reserve one. Neither a file descriptor nor a frame names a lock holder here.
///
/// # Errors
/// `Unsupported` off macOS; `InvalidInput` for an incomplete identity; `NotFound` when its
/// same-UID identity no longer matches; privacy, report I/O or size errors; or a failed,
/// refused or timed-out sample command; `Interrupted` on cancellation. No successful result
/// survives a failed post-check or a cancellation observed before return.
pub fn sample(
    expected: Identity,
    output_dir: &Path,
    canceled: impl Fn() -> bool,
) -> io::Result<Sample> {
    #[cfg(target_os = "macos")]
    {
        macos::sample(expected, output_dir, canceled)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (expected, output_dir, canceled);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "explicit wait sampling requires macOS",
        ))
    }
}

#[cfg(any(target_os = "macos", test))]
const KERNEL_PATH: &str = "/usr/lib/system/libsystem_kernel.dylib";
#[cfg(any(target_os = "macos", test))]
const KERNEL_NAME: &str = "libsystem_kernel.dylib";
#[cfg(any(target_os = "macos", test))]
const C_PATH: &str = "/usr/lib/system/libsystem_c.dylib";
#[cfg(any(target_os = "macos", test))]
const C_NAME: &str = "libsystem_c.dylib";
#[cfg(any(target_os = "macos", test))]
const MAX_REPORT: usize = 4 * 1024 * 1024;

#[cfg(any(target_os = "macos", test))]
fn identity_matches(expected: Identity, current: Option<Identity>) -> bool {
    expected.pid > 1 && expected.idversion.is_some() && current == Some(expected)
}

#[cfg(any(target_os = "macos", test))]
struct Image {
    name: &'static str,
    path: &'static str,
    uuid: String,
    start: u64,
    end: u64,
}

#[cfg(any(target_os = "macos", test))]
fn hex(word: &str) -> Option<u64> {
    let digits = word.strip_prefix("0x")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u64::from_str_radix(digits, 16).ok())
        .flatten()
}

#[cfg(any(target_os = "macos", test))]
fn mapping(images: &str, trusted: &Image) -> Option<(u64, u64)> {
    let mut found = None;
    for line in images.lines() {
        let words: Vec<_> = line.split_ascii_whitespace().collect();
        if words.last() != Some(&trusted.path) {
            continue;
        }
        if words.len() < 7 || words[1] != "-" || words[3] != trusted.name {
            return None;
        }
        let (start, end) = hex(words[0]).zip(hex(words[2]))?;
        let uuid = words[words.len() - 2];
        if start != trusted.start
            || start > end
            || end > trusted.end
            || uuid.strip_prefix('<').and_then(|u| u.strip_suffix('>'))
                != Some(trusted.uuid.as_str())
            || found.replace((start, end)).is_some()
        {
            return None;
        }
    }
    found
}

#[cfg(any(target_os = "macos", test))]
struct Frame<'a> {
    symbol: &'a str,
    image: &'a str,
    pc: u64,
}

#[cfg(any(target_os = "macos", test))]
fn frame(text: &str) -> Option<Frame<'_>> {
    let (symbol, rest) = text.split_once("  (in ")?;
    let (image, rest) = rest.split_once(')')?;
    let pc = hex(rest.rsplit_once("  [")?.1.strip_suffix(']')?)?;
    Some(Frame { symbol, image, pc })
}

/// The report is untrusted symbol text. Even a recognized leaf needs its address inside the
/// one kernel image whose path, UUID and code mapping match the reader's loaded Apple library.
#[cfg(any(target_os = "macos", test))]
fn classify(report: &str, kernel_image: &Image, c_image: Option<&Image>) -> Kind {
    if report.len() > MAX_REPORT || kernel_image.uuid.is_empty() {
        return Kind::Unparsed;
    }
    let mut image_sections = report.split("\nBinary Images:\n");
    let Some(before_images) = image_sections.next() else {
        return Kind::Unparsed;
    };
    let Some(images) = image_sections.next() else {
        return Kind::Unparsed;
    };
    if image_sections.next().is_some() {
        return Kind::Unparsed;
    }
    let Some((start, end)) = mapping(images, kernel_image) else {
        return Kind::Unparsed;
    };
    let c_mapping = c_image.and_then(|trusted| mapping(images, trusted));
    let mut graphs = before_images.split("\nCall graph:\n");
    let _header = graphs.next();
    let Some(graph_and_totals) = graphs.next() else {
        return Kind::Unparsed;
    };
    if graphs.next().is_some() {
        return Kind::Unparsed;
    }
    let Some((graph, _)) = graph_and_totals.split_once("\nTotal number in stack") else {
        return Kind::Unparsed;
    };
    let mut previous: Option<(usize, Kind)> = None;
    let mut common = None;
    let mut threads = 0usize;
    let mut leaves = 0usize;
    let mut ancestors = Vec::<(usize, bool)>::new();
    for line in graph.lines().filter(|line| !line.trim_ascii().is_empty()) {
        let Some(at) = line.find(|c: char| c.is_ascii_digit()) else {
            return Kind::Unparsed;
        };
        if !line[..at]
            .bytes()
            .all(|b| matches!(b, b' ' | b'+' | b'!' | b':' | b'|'))
        {
            return Kind::Unparsed;
        }
        let Some((count, frame)) = line[at..].split_once(' ') else {
            return Kind::Unparsed;
        };
        if count.parse::<u64>().ok().is_none_or(|count| count == 0) {
            return Kind::Unparsed;
        }
        let frame = frame.trim_ascii_start();
        let thread = frame.starts_with("Thread_");
        if thread {
            threads += 1;
        } else if threads == 0 {
            return Kind::Unparsed;
        }
        if let Some((depth, kind)) = previous
            && (thread || at <= depth)
        {
            leaves += 1;
            if kind == Kind::Unparsed || common.is_some_and(|old| old != kind) {
                return Kind::Unparsed;
            }
            common = Some(kind);
        }
        if thread {
            if previous.is_none() && threads > 1 {
                return Kind::Unparsed;
            }
            previous = None;
            ancestors.clear();
            continue;
        }
        while ancestors.last().is_some_and(|(depth, _)| *depth >= at) {
            ancestors.pop();
        }
        let nanosleep_parent = ancestors.last().is_some_and(|(_, nanosleep)| *nanosleep);
        let parsed = self::frame(frame);
        let nanosleep = parsed.as_ref().is_some_and(|frame| {
            frame.image == C_NAME
                && matches!(frame.symbol, "nanosleep" | "nanosleep$NOCANCEL")
                && c_mapping.is_some_and(|(start, end)| (start..=end).contains(&frame.pc))
        });
        let kind = parsed.and_then(|frame| {
            (frame.image == KERNEL_NAME && (start..=end).contains(&frame.pc)).then(|| {
                if nanosleep_parent
                    && matches!(
                        frame.symbol,
                        "__semwait_signal" | "__semwait_signal_nocancel"
                    )
                {
                    Kind::Timer
                } else {
                    leaf(frame.symbol)
                }
            })
        });
        ancestors.push((at, nanosleep));
        previous = Some((at, kind.unwrap_or(Kind::Unparsed)));
    }
    if let Some((_, kind)) = previous {
        leaves += 1;
        if kind == Kind::Unparsed || common.is_some_and(|old| old != kind) {
            return Kind::Unparsed;
        }
        common = Some(kind);
    }
    if leaves < threads {
        return Kind::Unparsed;
    }
    common.unwrap_or(Kind::Unparsed)
}

#[cfg(any(target_os = "macos", test))]
fn leaf(symbol: &str) -> Kind {
    match symbol {
        // __semwait_signal may be an untimed semaphore wait; its arguments are absent.
        // fcntl also performs non-lock operations; the report supplies no command argument.
        "flock" => Kind::Lock,
        "read" | "readv" | "__read_nocancel" | "__readv_nocancel" => Kind::Read,
        "select" | "pselect" | "poll" | "__select" | "__select_nocancel" | "__poll"
        | "__poll_nocancel" | "kevent" | "kevent64" | "kevent_qos" => Kind::Event,
        "mach_msg_trap" | "mach_msg2_trap" | "mach_msg_overwrite_trap" => Kind::Ipc,
        "__psynch_mutexwait" | "__psynch_cvwait" | "__psynch_rw_rdlock" | "__psynch_rw_wrlock"
        | "__ulock_wait" | "__ulock_wait2" => Kind::Mutex,
        "wait4" | "__wait4" | "__wait4_nocancel" => Kind::Children,
        _ => Kind::Unparsed,
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        C_NAME, C_PATH, Identity, Image, KERNEL_NAME, KERNEL_PATH, MAX_REPORT, Path, Sample,
        classify, identity_matches, io, retain_report,
    };
    use std::ffi::{CStr, c_void};
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::time::{Duration, Instant};

    fn check(expected: Identity) -> io::Result<()> {
        if expected.pid <= 1 || expected.idversion.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sampling needs an exact pid, birth and exec version",
            ));
        }
        if !identity_matches(expected, crate::process::read_identity(expected.pid)) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the same-UID process identity is no longer current",
            ));
        }
        Ok(())
    }

    fn check_cancellation(canceled: &impl Fn() -> bool) -> io::Result<()> {
        if canceled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "the wait sample was canceled",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn sample(
        expected: Identity,
        output_dir: &Path,
        canceled: impl Fn() -> bool,
    ) -> io::Result<Sample> {
        check_cancellation(&canceled)?;
        check(expected)?;
        crate::private_file::check_private_dir(output_dir)?;
        let name = format!("wait-{}.sample.txt", crate::rand::hex_token::<16>()?);
        let report = output_dir.join(&name);
        // Reserve a fresh 0600 file before sample opens it; the report is retained on error.
        let _reserved = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(crate::atomic_file::LOCK_OPEN_FLAGS)
            .open(&report)?;
        let began = Instant::now();
        let mut command = crate::spawnfd::command("/usr/bin/sample");
        command
            .arg(expected.pid.to_string())
            .args(["1", "100", "-file"])
            .arg(&report)
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null());
        // Recheck after private-file setup, immediately before the only sample invocation. Once
        // the report exists, every error carries its path so a diagnostic caller can retain it.
        check(expected).map_err(|error| retain_report(error, &report))?;
        check_cancellation(&canceled).map_err(|error| retain_report(error, &report))?;
        let status = run(&mut command, &canceled).map_err(|error| retain_report(error, &report))?;
        check(expected).map_err(|error| retain_report(error, &report))?;
        check_cancellation(&canceled).map_err(|error| retain_report(error, &report))?;
        if !status.success() {
            return Err(retain_report(
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the sample tool failed or refused the process",
                ),
                &report,
            ));
        }
        let bytes = crate::private_file::read_private(output_dir, &name, MAX_REPORT)
            .map_err(io::Error::other)
            .map_err(|error| retain_report(error, &report))?;
        let kernel = system_image(libc::read as *const c_void, KERNEL_NAME, KERNEL_PATH);
        let c_image = system_image(libc::nanosleep as *const c_void, C_NAME, C_PATH);
        let kind = std::str::from_utf8(&bytes)
            .ok()
            .zip(kernel)
            .map_or(super::Kind::Unparsed, |(text, kernel)| {
                classify(text, &kernel, c_image.as_ref())
            });
        check(expected).map_err(|error| retain_report(error, &report))?;
        check_cancellation(&canceled).map_err(|error| retain_report(error, &report))?;
        Ok(Sample {
            kind,
            report,
            elapsed: began.elapsed(),
        })
    }

    fn run(
        command: &mut crate::spawnfd::Command,
        canceled: &impl Fn() -> bool,
    ) -> io::Result<std::process::ExitStatus> {
        let by = Instant::now() + Duration::from_secs(60);
        let mut child = command.spawn()?;
        let mut exit = crate::deadline::ExitWait::new(&child);
        loop {
            let result = if canceled() {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "the wait sample was canceled",
                ))
            } else if Instant::now() >= by {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the sample tool did not finish",
                ))
            } else {
                child.try_wait()
            };
            match result {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => exit.wait(
                    by.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(25)),
                ),
                Err(error) => {
                    crate::deadline::kill_within(&mut child, Duration::from_millis(100));
                    return Err(error);
                }
            }
        }
    }

    /// An actual loaded Apple library's identity, never a target-provided image label.
    fn system_image(
        address: *const c_void,
        name: &'static str,
        path: &'static str,
    ) -> Option<Image> {
        let info = crate::darwin_loader::lookup(address)?;
        if info.fname.is_null() || info.fbase.is_null() {
            return None;
        }
        // SAFETY: dladdr's non-null image name is a loader-owned NUL-terminated string.
        if unsafe { CStr::from_ptr(info.fname) }.to_bytes() != path.as_bytes() {
            return None;
        }
        let base = info.fbase.cast::<u8>();
        // SAFETY: dladdr names this loaded Mach-O's header, which has at least 32 bytes.
        let (magic, ncmds, size) = unsafe {
            (
                base.cast::<u32>().read_unaligned(),
                base.add(16).cast::<u32>().read_unaligned(),
                base.add(20).cast::<u32>().read_unaligned(),
            )
        };
        if magic != 0xfeed_facf || ncmds > 512 || size > 1024 * 1024 {
            return None;
        }
        // SAFETY: this is the trusted, loaded system image's bounded load-command region.
        let commands = unsafe { std::slice::from_raw_parts(base.add(32), size as usize) };
        let mut at = 0usize;
        let mut uuid = None;
        let mut text_size = None;
        for _ in 0..ncmds {
            let header = commands.get(at..at.checked_add(8)?)?;
            let cmd = u32::from_ne_bytes(header[..4].try_into().ok()?);
            let size = u32::from_ne_bytes(header[4..].try_into().ok()?) as usize;
            if size < 8 {
                return None;
            }
            let command = commands.get(at..at.checked_add(size)?)?;
            if cmd == 0x19 && command.get(8..24)? == b"__TEXT\0\0\0\0\0\0\0\0\0\0" {
                let bytes = command.get(32..40)?.try_into().ok()?;
                let size = u64::from_ne_bytes(bytes);
                if size == 0 || size > 64 * 1024 * 1024 || text_size.replace(size).is_some() {
                    return None;
                }
            }
            if cmd == 0x1b {
                let bytes = command.get(8..24)?;
                let mut out = String::with_capacity(36);
                for (i, byte) in bytes.iter().enumerate() {
                    use std::fmt::Write as _;
                    if matches!(i, 4 | 6 | 8 | 10) {
                        out.push('-');
                    }
                    write!(out, "{byte:02X}").ok()?;
                }
                if uuid.replace(out).is_some() {
                    return None;
                }
            }
            at += size;
        }
        let start = u64::try_from(info.fbase as usize).ok()?;
        Some(Image {
            name,
            path,
            uuid: uuid?,
            start,
            end: start.checked_add(text_size?.checked_sub(1)?)?,
        })
    }
}

#[cfg(test)]
#[path = "wait_sample_tests.rs"]
mod tests;
