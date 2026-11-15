// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Collect only at the original owned child's reaping boundary. No PID-based
//! sampler, separate waiter or decoded observation can supply these fields.
use aterm_types::resource_admission::{AccountingObservation, AccountingScope, CpuTimes};
use aterm_uds::spawnfd::{Child, Rusage};
use std::{io, process::ExitStatus};

pub(crate) const fn supported() -> bool {
    cfg!(all(
        target_pointer_width = "64",
        any(target_os = "macos", target_os = "linux")
    ))
}

fn observation(usage: Rusage) -> AccountingObservation {
    let cpu_times = u64::try_from(usage.user.as_nanos())
        .ok()
        .zip(u64::try_from(usage.system.as_nanos()).ok())
        .map(|(user_ns, system_ns)| CpuTimes { user_ns, system_ns });
    AccountingObservation {
        scope: AccountingScope::ChildAndWaitedDescendantsV1,
        cpu_times,
        peak_rss_bytes: Some(usage.maxrss),
    }
}

/// Reap the same original child once. Unsupported accounting retains its real
/// status with explicitly absent fields; ownership errors never fabricate one.
pub(crate) fn try_reap(
    child: &mut Child,
    _accounted: bool,
) -> io::Result<Option<(ExitStatus, AccountingObservation)>> {
    // The resource monitor always retains the original owning wait's account. Public
    // lifecycle encoding still depends on the caller's explicitly negotiated protocol.
    let mut observed = crate::resource_observation::native_observation_guard();
    // WNOWAIT establishes exit while the original child remains unreaped. A running child's
    // earlier counter can never be mislabeled final because it exits during try_wait.
    let exited = match child.exited() {
        Ok(false) => return Ok(None),
        Ok(true) => true,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => false,
        Err(error) => return Err(error),
    };
    let exit_cut_ns = exited.then(aterm_uds::process::continuous_ns).flatten();
    let final_usage = exited
        .then(|| aterm_sysprobe::procs::process_usage(child.id()))
        .flatten();
    let result = match child.try_wait_with_rusage() {
        Ok(usage) => Ok(usage.map(|usage| (usage.status, observation(usage)))),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => child
            .try_wait()
            .map(|status| status.map(|status| (status, AccountingObservation::uncollected()))),
        Err(error) => Err(error),
    };
    if let Ok(Some((_, accounting))) = &result {
        observed.reaped_at(child.id(), *accounting, final_usage, exit_cut_ns);
    }
    result
}

#[cfg(all(
    test,
    target_pointer_width = "64",
    any(target_os = "macos", target_os = "linux")
))]
#[path = "resource_accounting_tests.rs"]
mod tests;
