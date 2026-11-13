// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! No-video CPU observations of one owned private headless instance.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::band::Band;
use crate::cpu_observation::{self, Args as Observation, Mode};
use crate::instance::{self, BOOT, Instance, Launch, World};
use crate::paths::from_caller;
use crate::{Fail, Res, stop};

pub const HELP: &str = "usage: targo --unverified lab idle-cpu --output FILE [--aterm PATH] [--duration-ms N] [--interval-ms N] [--settle-ms N] [--band app|agent]\n\nObserve CPU in one private headless instance with an idle /bin/cat pane, without video.\nThe standard lab configuration disables update/package/harness automation. Defaults:65s\nobserved after10s settle,50ms post-read sleeps, app band. On the live LAN use a debug\nbuild; release comparisons belong in a VM. FILE is created exclusively, never replaced.\n\nJSONL retains process/image identity, actual observation times, all observed thread\ncounters, incomplete native reads, and metrics at the boundaries only. Late, missing,\nrenamed or reset counters have no exact full-window delta. CPU-active sampled intervals\nare not wake counts. No quiet-host guarantee, acceptance threshold or performance verdict.\nThe instance ends and is reaped on completion, error or interruption.\n";

struct Args {
    aterm: Option<String>,
    output: PathBuf,
    duration: Duration,
    interval: Duration,
    settle: Duration,
    band: Band,
}

fn parse(args: &[String]) -> Result<Args, Fail> {
    let usage = || Fail::usage(HELP);
    let (pairs, remainder) = args.as_chunks::<2>();
    let mut values = BTreeMap::new();
    for pair in pairs {
        if ![
            "--aterm",
            "--output",
            "--duration-ms",
            "--interval-ms",
            "--settle-ms",
            "--band",
        ]
        .contains(&pair[0].as_str())
            || values.insert(pair[0].as_str(), pair[1].as_str()).is_some()
        {
            return Err(usage());
        }
    }
    if !remainder.is_empty() {
        return Err(usage());
    }
    let millis = |key, default| -> Result<Duration, Fail> {
        let n = values
            .get(key)
            .map(|text| text.parse::<u64>())
            .transpose()
            .map_err(|_| usage())?
            .unwrap_or(default);
        Ok(Duration::from_millis(n))
    };
    let duration = millis("--duration-ms", 65_000)?;
    let interval = millis("--interval-ms", 50)?;
    let settle = millis("--settle-ms", 10_000)?;
    if duration.is_zero()
        || interval.is_zero()
        || interval > duration
        || duration
            .checked_add(settle)
            .and_then(|total| Instant::now().checked_add(total))
            .is_none()
    {
        return Err(usage());
    }
    Ok(Args {
        aterm: values.get("--aterm").map(|v| (*v).to_owned()),
        output: PathBuf::from(
            *values
                .get("--output")
                .filter(|v| !v.is_empty())
                .ok_or_else(usage)?,
        ),
        duration,
        interval,
        settle,
        band: Band::pick(values.get("--band").copied())?,
    })
}

pub fn cmd(args: &[String]) -> Res {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        std::io::stdout()
            .write_all(HELP.as_bytes())
            .map_err(|e| e.to_string())?;
        return Ok(0);
    }
    stop::install();
    let args = parse(args)?;
    let aterm =
        std::fs::canonicalize(instance::resolve_aterm(args.aterm)?).map_err(|e| e.to_string())?;
    let output = PathBuf::from(from_caller(&args.output.to_string_lossy()));
    if output.try_exists().map_err(|e| e.to_string())? {
        return Err(Fail::die("idle-cpu: output already exists"));
    }
    let world = World::new(instance::CONFIG_OFF)?;
    let mut inst = Instance::start_in(
        &world,
        Launch {
            aterm: aterm.clone(),
            headless: true,
            args: ["--columns", "110", "--lines", "32", "-e", "/bin/cat"]
                .map(String::from)
                .to_vec(),
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        args.band,
    )?;
    let result = (|| {
        inst.await_socket(BOOT)?;
        let ready = instance::terminal_request(|a| inst.ctl(a), &["ready"]);
        if ready.code != 0 {
            return Err(Fail::die(format!("idle-cpu: not ready: {}", ready.first())));
        }
        stop::sleep(args.settle)?;
        cpu_observation::observe(Observation {
            pid: inst
                .aterm_pid()
                .ok_or("idle-cpu: instance identity unavailable")?,
            aterm,
            sock: inst.sock.clone(),
            output,
            duration: args.duration,
            interval: args.interval,
            mode: Mode::Idle {
                settle: args.settle,
                band: args.band,
            },
        })
    })();
    let result = instance::after_stop(result, inst.stop());
    if result.is_ok() {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "idle-cpu: observation complete").map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
    result
}

#[cfg(test)]
#[path = "idle_cpu_tests.rs"]
mod tests;
