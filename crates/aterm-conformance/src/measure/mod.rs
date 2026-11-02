// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `measure` check's instruments.

pub mod aos;
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
use std::sync::Mutex;

static ONE_TAKE: Mutex<()> = Mutex::new(());

/// What a process has used since it started, as `proc_pid_rusage` (`RUSAGE_INFO_V4`) reads it.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
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
    /// The release `aterm` named by the first of `vars` that is set.
    /// # Panics
    /// None of `vars` is set, or it names no file: a measuring row with nothing to measure is red,
    /// never "not measured".
    #[must_use]
    pub fn release(vars: &[&str], fake_tui: &str) -> Self {
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
        Self {
            aterm,
            fake_tui: PathBuf::from(fake_tui),
        }
    }
}
