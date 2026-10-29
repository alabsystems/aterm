// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observe a private registration, kill its parent, and require the helper to end.

#[cfg(all(target_os = "macos", debug_assertions))]
mod probe {
    use std::ffi::CString;
    use std::io::{BufRead, BufReader, Write};
    use std::ops::ControlFlow;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use aterm_uds::exitwatch::ExitWatch;
    use aterm_uds::process::{self, EndControlLive, Identity};
    use aterm_uds::spawnfd::{Child, Stdio, command};

    const SERVICE: &str = "_aterm-life._tcp";
    const WAIT: Duration = Duration::from_secs(60);

    struct Parent(Child);
    impl Drop for Parent {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct Registration {
        identity: Identity,
        exit: ExitWatch,
    }
    impl Drop for Registration {
        fn drop(&mut self) {
            if self.exit.exit_status().is_none() {
                let ended = process::end_proven(
                    self.identity,
                    process::END_GRACE,
                    &mut EndControlLive,
                    |_| true,
                );
                eprintln!("registration_cleanup={ended:?}");
            }
        }
    }

    fn parent(name: &str) -> Result<(), String> {
        let args = ["-R", name, SERVICE, "local.", "65534", "probe=1"].map(str::to_owned);
        let mut child = aterm_agent::node_service::spawn_alab_advertisement_for_probe(&args)
            .map_err(|e| e.to_string())?;
        println!("{}", child.id());
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        // The supervisor ends this parent with SIGKILL after observing its registration.
        child.wait().map_err(|e| e.to_string())?;
        Ok(())
    }

    fn supervise() -> Result<(), String> {
        let deadline = Instant::now() + WAIT;
        let name = format!("aterm-life-{}", std::process::id());
        let heard_name = name.clone();
        let (heard, ready) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("advertiser-probe-browse".into())
            .spawn(move || {
                let service = CString::new(SERVICE).expect("constant service");
                let result = aterm_net::bonjour::browse(&service, c"local.", |event| {
                    if event.added && event.name.to_bytes() == heard_name.as_bytes() {
                        let _ = heard.send(Ok(()));
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                });
                if let Err(error) = result {
                    let _ = heard.send(Err(error.to_string()));
                }
            })
            .map_err(|e| e.to_string())?;
        let mut parent = Parent(
            command(std::env::current_exe().map_err(|e| e.to_string())?)
                .args(["--parent", &name])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?,
        );
        let output = parent.0.stdout.take().ok_or("parent has no output")?;
        let (reported, report) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("advertiser-probe-parent".into())
            .spawn(move || {
                let mut line = String::new();
                let read = BufReader::new(output).read_line(&mut line);
                let _ = reported.send(read.map(|_| line));
            })
            .map_err(|e| e.to_string())?;
        let pid: i32 = report
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?
            .trim()
            .parse()
            .map_err(|e| format!("registration pid: {e}"))?;
        let identity = process::read_identity(pid).ok_or("registration identity unavailable")?;
        let exit = ExitWatch::watch(u32::try_from(pid).map_err(|e| e.to_string())?)
            .ok_or("cannot watch registration exit")?;
        let mut registration = Registration { identity, exit };
        ready
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|e| e.to_string())??;
        // A tether execs the registrar after spawn returns. Keep the identity of
        // that running image for cleanup, rather than its earlier tether image.
        registration.identity =
            process::read_identity(pid).ok_or("registered helper identity unavailable")?;
        if process::read_process_parent(pid) != Some(parent.0.id()) {
            return Err("registration is not this probe parent's child".into());
        }
        println!(
            "observed={name} parent={} registration={pid}",
            parent.0.id()
        );
        parent.0.kill().map_err(|e| e.to_string())?;
        let status = parent.0.wait().map_err(|e| e.to_string())?;
        println!("parent_exit={status}");
        // A required exit has a one-minute hang detector, never a latency budget.
        loop {
            if let Some(status) = registration.exit.exit_status() {
                println!("registration_exit_status={status}");
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("registration outlived its SIGKILLed parent".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub(super) fn run() -> Result<(), String> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        match args.as_slice() {
            [] => supervise(),
            [flag, name] if flag == "--parent" => parent(name),
            _ => Err("usage: advertiser_lifetime".into()),
        }
    }
}

fn main() -> std::process::ExitCode {
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    #[cfg(all(target_os = "macos", debug_assertions))]
    {
        match probe::run() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                std::process::ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(all(target_os = "macos", debug_assertions)))]
    {
        eprintln!("advertiser_lifetime needs a macOS debug build");
        std::process::ExitCode::from(3)
    }
}
