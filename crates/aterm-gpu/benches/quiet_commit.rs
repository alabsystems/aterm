// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Times the first commit after the GPU went quiet, cold and pre-warmed (ATERM-KI-1027-4):
//! `targo --unverified bench -p aterm-gpu --bench quiet_commit`. Each repetition runs every
//! (quiet, lead) pair once in its own shuffled order. `QUIET_COMMIT_REPS` sets the repetitions
//! (default 12) and `QUIET_COMMIT_SIZE` the timed target (`WxH`, default 1760x1024, about a
//! 110x32 window's drawable at 2x). It prints one `TAKE` line per take, then one `ARM` line per
//! pair with the medians and 95th percentiles over its takes.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("quiet_commit: could not run: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

/// The quiets each repetition runs, ms: back to back, then past the driver's release.
#[cfg(target_os = "macos")]
const QUIETS_MS: [u64; 9] = [0, 100, 250, 500, 1_000, 1_500, 2_000, 3_000, 5_000];

/// The pre-warm leads each quiet runs under, us: none, two echo-sized gaps, and one past the
/// driver's re-take.
#[cfg(target_os = "macos")]
const LEADS_US: [Option<u64>; 4] = [None, Some(500), Some(2_000), Some(8_000)];

#[cfg(target_os = "macos")]
fn run() -> Result<(), String> {
    use aterm_gpu::{QuietTake, quiet_commit_probe};
    use std::time::Duration;
    let reps: usize = env_or("QUIET_COMMIT_REPS", 12)?;
    let size = match std::env::var("QUIET_COMMIT_SIZE") {
        Ok(s) => {
            let (w, h) = s
                .split_once('x')
                .ok_or_else(|| format!("QUIET_COMMIT_SIZE={s:?} is not WxH"))?;
            (
                w.parse().map_err(|e| format!("width {w:?}: {e}"))?,
                h.parse().map_err(|e| format!("height {h:?}: {e}"))?,
            )
        }
        Err(_) => (1760, 1024),
    };
    let pairs: Vec<QuietTake> = QUIETS_MS
        .iter()
        .flat_map(|&q| {
            LEADS_US.iter().map(move |&l| QuietTake {
                quiet: Duration::from_millis(q),
                lead: l.map(Duration::from_micros),
            })
        })
        .collect();
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut plan = Vec::with_capacity(pairs.len() * reps);
    for _ in 0..reps {
        let mut order = pairs.clone();
        for i in (1..order.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let j = usize::try_from(seed % (i as u64 + 1)).expect("an index");
            order.swap(i, j);
        }
        plan.extend(order);
    }
    let takes = quiet_commit_probe(size, &plan, &footprint)?;
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    let mib =
        |b: Option<u64>| b.map_or("-".to_owned(), |b| format!("{:.1}", b as f64 / 1_048_576.0));
    for t in &takes {
        println!(
            "TAKE quiet_ms={} lead_us={} lead_real_us={} warm_us={} encode_us={:.1} commit_us={:.1} \
             submit_us={:.1} complete_us={:.1} gpu_us={:.1} fp_quiet_mib={} fp_done_mib={}",
            t.take.quiet.as_millis(),
            t.take
                .lead
                .map_or("-".to_owned(), |l| l.as_micros().to_string()),
            t.lead.map_or("-".to_owned(), |l| format!("{:.1}", us(l))),
            t.warm.map_or("-".to_owned(), |w| format!("{:.1}", us(w))),
            us(t.encode),
            us(t.commit),
            us(t.encode + t.commit),
            us(t.complete),
            us(t.gpu),
            mib(t.footprint_quiet),
            mib(t.footprint_done),
        );
    }
    for pair in &pairs {
        let of: Vec<_> = takes.iter().filter(|t| t.take == *pair).collect();
        let pct = |mut v: Vec<f64>, p: f64| {
            v.sort_by(f64::total_cmp);
            let at = ((v.len() as f64 - 1.0) * p).round() as usize;
            v.get(at).copied().unwrap_or(f64::NAN)
        };
        let submit: Vec<f64> = of.iter().map(|t| us(t.encode + t.commit)).collect();
        let complete: Vec<f64> = of.iter().map(|t| us(t.complete)).collect();
        let gpu: Vec<f64> = of.iter().map(|t| us(t.gpu)).collect();
        let fp: Vec<f64> = of
            .iter()
            .filter_map(|t| t.footprint_quiet)
            .map(|b| b as f64 / 1_048_576.0)
            .collect();
        println!(
            "ARM quiet_ms={} lead_us={} n={} submit_p50_us={:.1} submit_p95_us={:.1} \
             complete_p50_us={:.1} complete_p95_us={:.1} gpu_p50_us={:.1} fp_quiet_p50_mib={:.1}",
            pair.quiet.as_millis(),
            pair.lead
                .map_or("-".to_owned(), |l| l.as_micros().to_string()),
            of.len(),
            pct(submit.clone(), 0.5),
            pct(submit, 0.95),
            pct(complete.clone(), 0.5),
            pct(complete, 0.95),
            pct(gpu, 0.5),
            pct(fp, 0.5),
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn env_or(name: &str, default: usize) -> Result<usize, String> {
    match std::env::var(name) {
        Ok(v) => v.parse().map_err(|e| format!("{name}={v:?}: {e}")),
        Err(_) => Ok(default),
    }
}

/// This process's physical footprint, bytes (`ri_phys_footprint`, Activity Monitor's Memory).
#[cfg(target_os = "macos")]
fn footprint() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `proc_pid_rusage` writes at most one `rusage_info_v4` for `RUSAGE_INFO_V4` into the
    // zeroed buffer `info` points at, which outlives the call.
    let rc = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: zeroed at creation, so initialized whatever the call wrote.
    Some(unsafe { info.assume_init() }.ri_phys_footprint)
}

/// The Metal arm exists only on macOS.
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("quiet_commit: the Metal arm is macOS's; nothing to measure here");
}
