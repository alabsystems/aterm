// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flavor choice on stand-in kernels, and the V6 read on this one.

use super::*;

/// A stand-in kernel: each call's flavor is recorded; `refuse_v6` answers `RUSAGE_INFO_V6`
/// with that errno, and a call that is answered writes its flavor into the record.
fn kernel(
    calls: &mut Vec<c_int>,
    refuse_v6: Option<i32>,
) -> impl FnMut(c_int, &mut u64) -> Result<(), i32> + '_ {
    move |flavor, record| {
        calls.push(flavor);
        match refuse_v6 {
            Some(errno) if flavor == RUSAGE_INFO_V6 => Err(errno),
            _ => {
                *record = u64::try_from(flavor).expect("a flavor");
                Ok(())
            }
        }
    }
}

/// A kernel with V6 answers it, and every later read asks V6 first.
#[test]
fn a_kernel_with_v6_is_read_as_v6() {
    let flavor = Flavor::new();
    let mut calls = Vec::new();
    let mut record = 0;
    assert_eq!(flavor.read(&mut record, kernel(&mut calls, None)), Ok(true));
    assert_eq!(flavor.read(&mut record, kernel(&mut calls, None)), Ok(true));
    assert_eq!(record, 6);
    assert_eq!(calls, [RUSAGE_INFO_V6, RUSAGE_INFO_V6]);
}

/// A kernel before macOS 12 refuses V6 with `EINVAL`: the read falls back to V4 into the same
/// record, says it is no V6 (so its energy reads `None`), and every later read asks V4 alone.
#[test]
fn a_kernel_without_v6_falls_back_to_v4_once_and_remembers() {
    let flavor = Flavor::new();
    let mut calls = Vec::new();
    let mut record = 0;
    assert_eq!(
        flavor.read(&mut record, kernel(&mut calls, Some(EINVAL))),
        Ok(false)
    );
    assert_eq!(record, 4);
    assert_eq!(calls, [RUSAGE_INFO_V6, RUSAGE_INFO_V4]);
    calls.clear();
    assert_eq!(
        flavor.read(&mut record, kernel(&mut calls, Some(EINVAL))),
        Ok(false)
    );
    assert_eq!(calls, [RUSAGE_INFO_V4], "the refusal was learned");
}

/// NEGATIVE CONTROL: a refusal that is not `EINVAL` (another user's process, `EPERM`; a gone
/// one, `ESRCH`) is that read's answer, with no V4 retry, and teaches nothing: the next read asks
/// V6 again. Read as a missing flavor, one other user's process would turn energy off for good.
#[test]
fn a_refusal_other_than_einval_is_the_answer_and_teaches_nothing() {
    for errno in [1, 3] {
        let flavor = Flavor::new();
        let mut calls = Vec::new();
        let mut record = 0;
        assert_eq!(
            flavor.read(&mut record, kernel(&mut calls, Some(errno))),
            Err(errno)
        );
        assert_eq!(calls, [RUSAGE_INFO_V6]);
        calls.clear();
        assert_eq!(flavor.read(&mut record, kernel(&mut calls, None)), Ok(true));
        assert_eq!(calls, [RUSAGE_INFO_V6], "still asks V6 first");
    }
}

/// Once V6 answered, a later `EINVAL` is that read's failure, not a missing flavor.
#[test]
fn an_einval_after_v6_answered_is_a_failure() {
    let flavor = Flavor::new();
    let mut calls = Vec::new();
    let mut record = 0;
    assert_eq!(flavor.read(&mut record, kernel(&mut calls, None)), Ok(true));
    assert_eq!(
        flavor.read(&mut record, kernel(&mut calls, Some(EINVAL))),
        Err(EINVAL)
    );
    assert_eq!(calls, [RUSAGE_INFO_V6, RUSAGE_INFO_V6]);
}

/// Live: this Mac's kernel answers V6 for this process, with its energy, and the V4 prefix of
/// the V6 record is the record a V4 read gives (start and lifetime peak cannot move between the
/// reads). The sweep reads this process's energy too.
#[cfg(target_os = "macos")]
#[test]
fn this_kernel_reads_this_process_as_v6_with_its_energy() {
    let pid = std::process::id();
    let me = i32::try_from(pid).expect("a pid");
    let mut spun = 0u64;
    let until = std::time::Instant::now() + std::time::Duration::from_millis(50);
    while std::time::Instant::now() < until {
        spun = std::hint::black_box(spun.wrapping_add(1));
    }
    let usage = crate::procs::imp::rusage(me).expect("this process's usage");
    assert!(usage.v6, "macOS 12 and later answer RUSAGE_INFO_V6");
    assert!(
        usage.energy_nj().is_some_and(|nj| nj > 0),
        "50 ms of spinning used energy: {:?}",
        usage.energy_nj()
    );
    // SAFETY: `rusage_info_v4` is plain integers; all-zero is valid.
    let mut v4: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: `v4` is a writable `rusage_info_v4`, the flavor's record.
    let rc = unsafe {
        libc::proc_pid_rusage(
            me,
            RUSAGE_INFO_V4,
            (&raw mut v4).cast::<libc::rusage_info_t>(),
        )
    };
    assert_eq!(rc, 0);
    assert_eq!(usage.ri.ri_proc_start_abstime, v4.ri_proc_start_abstime);
    assert!(usage.ri.ri_user_time <= v4.ri_user_time);
    let sweep = crate::processes().expect("the sweep");
    let mine = sweep
        .procs
        .iter()
        .find(|p| p.pid == pid)
        .expect("the sweep lists this process");
    assert!(mine.energy_nj.is_some_and(|nj| nj > 0), "{mine:?}");
    assert!(spun > 0);
}
