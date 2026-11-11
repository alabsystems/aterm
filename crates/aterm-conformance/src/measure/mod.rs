// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `measure` check's instruments.

pub mod aos;
pub mod decision;
pub mod frame;
pub mod instance;
pub mod paint;
pub mod paint_probe;
pub mod rewind;
pub mod rewind_cost;
pub mod scan;
pub mod spin;
pub mod starvation;

use std::path::PathBuf;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

static TAKES: RwLock<()> = RwLock::new(());

/// Count instruments have private worlds. A timing instrument excludes every other take.
struct Take<'a> {
    _count: Option<RwLockReadGuard<'a, ()>>,
    _timing: Option<RwLockWriteGuard<'a, ()>>,
}

fn take(timing: bool) -> Take<'static> {
    take_on(&TAKES, timing)
}

fn take_on(gate: &RwLock<()>, timing: bool) -> Take<'_> {
    if timing {
        Take {
            _count: None,
            _timing: Some(
                gate.write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ),
        }
    } else {
        Take {
            _count: Some(
                gate.read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ),
            _timing: None,
        }
    }
}

/// What a process has used since it started, as `proc_pid_rusage` (`RUSAGE_INFO_V4`) reads it.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    /// Its pages in physical memory now, bytes (`ri_resident_size`), including clean file-backed
    /// pages; this is the resident-size reading reported beside the footprint.
    pub resident: u64,
    /// Its physical footprint now, bytes (`ri_phys_footprint`, Activity Monitor's "Memory"): its
    /// dirty private pages, compressed ones too, and none of the clean file-backed pages that
    /// `ps`'s resident size also counts.
    pub footprint: u64,
    /// Its user and system CPU time, ns: the kernel's Mach ticks (`ri_user_time`,
    /// `ri_system_time`) at the kernel's own timebase, 125/3 ns a tick on Apple silicon. No
    /// energy is read: the struct's `ri_billed_energy` does not follow the process's own work
    /// (spin takes of 2026-10-29 read 22 mJ for a bare window's 29 frames and 0.3 mJ for a pet
    /// window's 69).
    pub cpu_ns: u64,
}

/// `pid`'s [`Usage`] now.
/// # Errors
/// [`instance::CouldNotRun::Refused`] when the pid is out of range, the kernel names no usage for
/// it (it exited), or the kernel's timebase is unreadable.
#[cfg(target_os = "macos")]
pub fn usage(pid: u32) -> Result<Usage, instance::CouldNotRun> {
    use instance::CouldNotRun;
    let id = libc::c_int::try_from(pid)
        .map_err(|_| CouldNotRun::Refused(format!("pid {pid} is out of range")))?;
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `info` is a zeroed `rusage_info_v4`, the struct `RUSAGE_INFO_V4` fills; the
    // kernel writes at most its size, and it is read only on success.
    let rc = unsafe {
        libc::proc_pid_rusage(
            id,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return Err(CouldNotRun::Refused(format!(
            "proc_pid_rusage named no usage for {pid}"
        )));
    }
    // SAFETY: zeroed is a valid `rusage_info_v4` (plain integers), and the kernel filled it.
    let info = unsafe { info.assume_init() };
    let (numer, denom) = aterm_sysprobe::procs::kernel_timebase()
        .ok_or_else(|| CouldNotRun::Refused("the kernel's timebase is unreadable".into()))?;
    let ticks = info.ri_user_time.saturating_add(info.ri_system_time);
    Ok(Usage {
        resident: info.ri_resident_size,
        footprint: info.ri_phys_footprint,
        cpu_ns: aterm_sysprobe::procs::ticks_to_ns(ticks, numer, denom),
    })
}

/// What a row is measured with: the binary under measurement and the stand-in TUI the scenes run.
#[derive(Clone, Debug)]
pub struct Rig {
    pub aterm: PathBuf,
    pub fake_tui: PathBuf,
}

impl Rig {
    /// The release `aterm` named by the first of `vars` that is set, with the stand-in TUI at
    /// `fake_tui`.
    /// # Panics
    /// None of `vars` is set, or it names no file: a measuring row with nothing to measure is red,
    /// never "not measured".
    #[must_use]
    pub fn release(vars: &[&str], fake_tui: PathBuf) -> Self {
        let aterm = vars
            .iter()
            .find_map(|v| std::env::var_os(v).map(PathBuf::from))
            .unwrap_or_else(|| {
                panic!(
                    "this row measures a release aterm: set {} to its executable \
                     (<aterm.app>/Contents/MacOS/aterm)",
                    vars.join(" or ")
                )
            });
        assert!(aterm.is_file(), "{} is not a file", aterm.display());
        Self { aterm, fake_tui }
    }
}

/// Maximum size (128 MiB) accepted when reading PNG frames, recording indices, or logs.
pub(crate) const MAX_MEASURE_FILE_BYTES: u64 = 128 << 20;

pub(crate) fn open_regular_file_with_meta(
    path: &std::path::Path,
) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(not(unix))]
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

pub(crate) fn open_regular_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    open_regular_file_with_meta(path).map(|(file, _)| file)
}

pub(crate) fn read_regular_file(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let (file, meta) = open_regular_file_with_meta(path)?;
    if meta.len() > MAX_MEASURE_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds 128 MiB limit",
        ));
    }
    let cap =
        usize::try_from(meta.len().min(MAX_MEASURE_FILE_BYTES).saturating_add(1)).unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
    file.take(MAX_MEASURE_FILE_BYTES + 1)
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_MEASURE_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file grew past 128 MiB limit while reading",
        ));
    }
    Ok(buf)
}

pub(crate) fn read_regular_text(path: &std::path::Path) -> std::io::Result<String> {
    String::from_utf8(read_regular_file(path)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Write `bytes` to a fresh regular file at `path` (unlinking any existing entry first and
/// creating exclusively with `create_new(true)`).
pub fn write_fresh(
    path: impl AsRef<std::path::Path>,
    bytes: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = path.as_ref();
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    opts.open(path)?.write_all(bytes.as_ref())
}

pub(crate) fn create_stdio_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    opts.open(path)
}

#[cfg(test)]
#[path = "mod_take_tests.rs"]
mod take_tests;
