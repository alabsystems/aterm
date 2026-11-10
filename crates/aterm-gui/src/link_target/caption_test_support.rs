// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retired-work readings shared by the paired caption probes.

#[cfg(target_os = "macos")]
pub(crate) fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: the output has the size/alignment of RUSAGE_INFO_V4; read only on success.
    let rc = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: a successful proc_pid_rusage initialized the output.
    (rc == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn instructions() -> Option<u64> {
    None
}
