// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! File-event census with the same probe used by doctor. Optional roots filter the report.

fn footprint() -> Option<(u32, u64, u64)> {
    match aterm_uds::services::look(&aterm_vitals::services::FSEVENTSD) {
        aterm_uds::services::Look::Running(service, _) if service.live == 1 => {
            Some((service.pid, service.start_s, service.footprint))
        }
        _ => None,
    }
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args
        .next()
        .ok_or("usage: file_events <seconds> [--files] [root ...]")?
        .parse()
        .map_err(|_| "invalid seconds")?;
    let remaining: Vec<_> = args.collect();
    let exact = remaining.first().is_some_and(|arg| arg == "--files");
    let roots: Vec<_> = remaining
        .iter()
        .skip(usize::from(exact))
        .map(std::path::PathBuf::from)
        .collect();
    let before = footprint();
    let start = std::time::Instant::now();
    let duration = std::time::Duration::from_secs(seconds);
    let mode = if exact {
        aterm_sysprobe::file_events::Granularity::Files(&roots)
    } else {
        aterm_sysprobe::file_events::Granularity::Directories(&roots)
    };
    let census =
        aterm_sysprobe::file_events::capture(duration, mode, || eprintln!("sampling-ready"))?;
    let elapsed = start.elapsed().as_secs_f64();
    let after = footprint();
    println!(
        "seconds={elapsed:.3} dropped_markers={} overflow={} footprint_before={before:?} footprint_after={after:?}",
        census.dropped, census.overflow
    );
    for (path, count) in census.ranked(&roots) {
        println!("{count}\t{:.1}\t{path}", count as f64 * 3600.0 / elapsed);
    }
    Ok(())
}
