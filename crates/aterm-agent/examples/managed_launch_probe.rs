// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only native admission probe. No environment values, record contents or signals.
//! Usage: managed_launch_probe <pid>
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let pid: u32 = arguments.next().ok_or("expected pid")?.parse()?;
    if pid <= 1 || arguments.next().is_some() {
        return Err("expected one non-system pid".into());
    }
    let start =
        aterm_agent::harness::restart_stamp::kernel_start(pid).ok_or("kernel start unavailable")?;
    let args = atpkg::caller_shell::process_args(pid).ok_or("kernel arguments unavailable")?;
    let image = aterm_sysprobe::process_path(pid).ok_or("kernel image unavailable")?;
    let entry = args.argv.first().ok_or("argv0 unavailable")?;
    let layout = atpkg::store::resolve_configured().ok_or("configured store unavailable")?;
    let legacy = atpkg::managed_launch::Hints::capture(&layout, &args);
    let admitted = atpkg::managed_launch::Hints::capture_for_pid(&layout, &args, pid);
    let claim = aterm_agent::harness::self_claimed::read_copy(pid, &start, None)
        .ok_or("live claim snapshot unavailable")?;
    if aterm_agent::harness::restart_stamp::kernel_start(pid).as_deref() != Some(start.as_str()) {
        return Err("process identity changed".into());
    }
    let resume = claim
        .resume
        .map(|_| "admitted")
        .unwrap_or_else(|reason| reason);
    println!(
        "pid={pid} producer_fields_complete={} managed_route_admitted={} self_claim={resume}",
        legacy.is_some(),
        admitted.is_some_and(|hints| hints.admits(pid, &image, std::path::Path::new(entry)))
    );
    Ok(())
}
