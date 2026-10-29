// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live native-browse probe: SERVICE [--hold]. It observes service metadata only.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    use std::ffi::CString;
    use std::ops::ControlFlow;
    use std::process::ExitCode;
    use std::time::Duration;

    let mut args = std::env::args().skip(1);
    let Some(service) = args.next() else {
        eprintln!("usage: bonjour_probe SERVICE [--hold]");
        return ExitCode::from(2);
    };
    let hold = match args.next().as_deref() {
        None => false,
        Some("--hold") => true,
        _ => return ExitCode::from(2),
    };
    if args.next().is_some() {
        return ExitCode::from(2);
    }
    let Ok(service) = CString::new(service) else {
        return ExitCode::from(2);
    };
    // A hang detector, not a discovery latency requirement.
    if std::thread::Builder::new()
        .name("bonjour-probe-deadline".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(60));
            eprintln!("the probe's one-minute hang detector expired");
            std::process::exit(3);
        })
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    match aterm_net::bonjour::browse(&service, c"local.", |event| {
        println!(
            "event={} interface={} name={:?}",
            if event.added { "add" } else { "remove" },
            event.interface,
            event.name
        );
        if hold || !event.added {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("bonjour_probe needs macOS");
    std::process::ExitCode::from(3)
}
