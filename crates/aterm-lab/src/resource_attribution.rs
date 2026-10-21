// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real delegated work under one external launcher, against a private aterm.
//! The worker uses a new session while retaining its registered parent root.

use std::io::{BufRead, Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use crate::instance::{CONFIG_OFF, Instance, Launch, World};
use crate::screen::field;
use crate::{Fail, Res};

const HANG: Duration = Duration::from_secs(60);

pub fn cmd(args: &[String]) -> Res {
    match args.first().map(String::as_str) {
        Some("worker") => return worker(),
        Some("child") => return child(),
        Some("observer") => return observer(),
        _ => {}
    }
    let path = match args {
        [flag, path] if flag == "--aterm" => Some(path.clone()),
        [] => None,
        _ => return Err(Fail::usage("resource-attribution [--aterm PATH]")),
    };
    let aterm = crate::instance::resolve_aterm(path)?;
    let world = World::new(CONFIG_OFF)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Local process-name fixtures, never live AI services. The host discovers
    // their distinct programs through the same real PTY observation.
    for program in ["claude", "codex"] {
        std::os::unix::fs::symlink(&exe, world.home.join(program)).map_err(|e| e.to_string())?;
    }
    let mut instance = Instance::start(
        &world,
        Launch {
            aterm: aterm.clone(),
            headless: true,
            args: vec!["-e".into(), "/bin/sh".into()],
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
        },
    )?;
    instance.await_socket(HANG)?;
    let first = sessions(&instance)?;
    checked(&instance, &["spawn"])?;
    let both = sessions(&instance)?;
    let a = first
        .first()
        .ok_or("private instance has no initial session")?;
    let b = both
        .iter()
        .find(|sid| !first.contains(sid))
        .ok_or("spawn created no new session")?;
    for (sid, program, args) in [
        (a, "claude", "idle"),
        (b, "codex", "resource-attribution observer"),
    ] {
        // World::new chooses this canonical /tmp/at.<random>/home path itself.
        let command = format!("exec '{}' {args}", world.home.join(program).display());
        checked(&instance, &[&format!("@{sid}"), "send", &command])?;
        checked(&instance, &[&format!("@{sid}"), "key", "enter"])?;
    }
    let refused = instance.ctl(&[
        &format!("@{a}"),
        "resources",
        "register",
        "start=0",
        "job=refused",
    ]);
    if refused.code == 0 || !refused.stderr.contains("stale-peer") {
        return Err(Fail::die(format!(
            "stale kernel start was not refused: {} {}",
            refused.stdout, refused.stderr
        )));
    }
    if checked(&instance, &[&format!("@{a}"), "resources"])?
        .lines()
        .any(|line| line.starts_with("job "))
    {
        return Err(Fail::die("refused registration left a job"));
    }
    let mut jobs = Vec::new();
    for (sid, label, program) in [(a, "tab-a", "claude"), (b, "tab-b", "codex")] {
        // This is the external launcher, outside the private aterm's tree.
        // @self comes from the authenticated session selector's real variable;
        // the deliberately stale legacy variable must never select the tab.
        let until = Instant::now() + HANG;
        let (capabilities, ai) = loop {
            let output = aterm_uds::deadline::output_until(
                aterm_uds::spawnfd::command(&aterm)
                    .args([
                        "ctl",
                        "--sock",
                        &instance.sock.display().to_string(),
                        "@self",
                        "resources",
                        "capabilities",
                    ])
                    .env_clear()
                    .envs(world.env())
                    .env("ATERM_PARENT_SESSION_ID", sid)
                    .env("ATERM_SESSION_ID", "s-dead"),
                until,
            )
            .map_err(|e| format!("capability probe: {e:?}"))?;
            let capabilities = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.status.success()
                || field(&capabilities, "version").as_deref() != Some("1")
                || field(&capabilities, "available").as_deref() != Some("1")
                || field(&capabilities, "session").as_deref() != Some(sid)
            {
                return Err(Fail::die(format!(
                    "capability probe did not resolve {sid}: {capabilities}"
                )));
            }
            let ai = field(&capabilities, "ai").ok_or("capability reply omitted ai")?;
            if ai == program {
                break (capabilities, ai);
            }
            if Instant::now() >= until {
                return Err(Fail::die(format!(
                    "host did not observe the local {program} fixture: {capabilities}"
                )));
            }
            crate::stop::sleep(Duration::from_millis(100))?;
        };
        println!("capabilities tab={label} {}", capabilities.trim());
        let runner = field(&capabilities, "runner").ok_or("capability reply omitted runner")?;
        let runner = aterm_codec::pct::decode(&runner);
        let process = aterm_uds::spawnfd::command(runner)
            .args([
                "resource-run",
                "--sock",
                &instance.sock.display().to_string(),
                "--session",
                "@self",
                "--job",
                label,
                "--",
            ])
            .arg(&exe)
            .args(["resource-attribution", "child"])
            .env_clear()
            .envs(world.env())
            .env("ATERM_PARENT_SESSION_ID", sid)
            .env("ATERM_SESSION_ID", "s-dead")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| e.to_string())?;
        jobs.push(Job::new(process, sid.clone(), label, ai)?);
    }
    for job in &jobs {
        if job.root == std::process::id() || job.worker == job.root {
            return Err(Fail::die(
                "the workload did not produce an independent child",
            ));
        }
        println!(
            "launched tab={} session={} root={} worker={} daemon={}",
            job.label,
            job.sid,
            job.root,
            job.worker,
            std::process::id()
        );
    }
    let until = Instant::now() + HANG;
    loop {
        let mut complete = true;
        for job in &jobs {
            let rows = checked(&instance, &[&format!("@{}", job.sid), "resources"])?;
            let lines: Vec<_> = rows
                .lines()
                .filter(|line| line.starts_with("job "))
                .collect();
            if lines.len() != 1 {
                complete = false;
                continue;
            }
            let row = lines[0];
            if field(row, "root_pid").as_deref() != Some(&job.root.to_string())
                || field(row, "label").as_deref() != Some(job.label)
                || field(row, "ai").as_deref() != Some(&job.ai)
            {
                return Err(Fail::die(format!("cross-tab ownership: {row}")));
            }
            let procs = field(row, "procs").and_then(|v| v.parse::<usize>().ok());
            let cpu = field(row, "cpu_cores").and_then(|v| v.parse::<f64>().ok());
            let bytes = field(row, "physical_bytes").and_then(|v| v.parse::<u64>().ok());
            // Two processes exactly: the registered root and its setsid child.
            // No shared launcher or the other tab's two processes is included.
            if procs != Some(2)
                || !cpu.is_some_and(|v| v > 0.0)
                || !bytes.is_some_and(|v| v >= 16 * 1024 * 1024)
            {
                complete = false;
            } else {
                println!("observed tab={} {row}", job.label);
            }
        }
        if complete {
            break;
        }
        if Instant::now() >= until {
            return Err(Fail::die("delegated resource samples did not arrive"));
        }
        crate::stop::sleep(Duration::from_millis(100))?;
    }
    for job in &mut jobs {
        job.finish()?;
    }
    let until = Instant::now() + HANG;
    loop {
        let mut empty = true;
        for job in &jobs {
            let rows = checked(&instance, &[&format!("@{}", job.sid), "resources"])?;
            empty &= !rows.lines().any(|line| line.starts_with("job "));
        }
        if empty {
            break;
        }
        if Instant::now() >= until {
            return Err(Fail::die("dead delegated jobs were not pruned"));
        }
        crate::stop::sleep(Duration::from_millis(100))?;
    }
    println!(
        "PASS delegated resources: two external-daemon tabs, observed AI defaults, stale legacy env, setsid children, current CPU/physical footprint, exact roots and dead-job pruning"
    );
    Ok(0)
}

fn checked(instance: &Instance, args: &[&str]) -> Result<String, Fail> {
    let reply = instance.ctl(args);
    if reply.code != 0 {
        return Err(Fail::die(format!(
            "ctl {args:?}: {} {}",
            reply.stdout, reply.stderr
        )));
    }
    Ok(reply.stdout)
}

fn sessions(instance: &Instance) -> Result<Vec<String>, Fail> {
    let rows = checked(instance, &["sessions"])?;
    let sessions = rows
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            words.next()?.parse::<u64>().ok()?;
            let sid = words.next()?;
            sid.starts_with("s-").then(|| sid.to_string())
        })
        .collect();
    println!("sessions {rows}");
    Ok(sessions)
}

struct Job {
    process: aterm_uds::spawnfd::Child,
    reader: Option<std::thread::JoinHandle<()>>,
    root: u32,
    worker: u32,
    worker_start: Option<aterm_uds::process::ProcessStart>,
    sid: String,
    label: &'static str,
    ai: String,
}

impl Job {
    fn new(
        mut process: aterm_uds::spawnfd::Child,
        sid: String,
        label: &'static str,
        ai: String,
    ) -> Result<Self, Fail> {
        let stdout = process.stdout.take().ok_or("no workload stdout")?;
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                if tx.send(line.unwrap_or_default()).is_err() {
                    break;
                }
            }
        });
        let mut job = Self {
            root: process.id(),
            worker: 0,
            worker_start: None,
            process,
            reader: Some(reader),
            sid,
            label,
            ai,
        };
        let line = rx
            .recv_timeout(HANG)
            .map_err(|e| format!("worker start: {e}"))?;
        job.worker = field(&line, "worker")
            .and_then(|v| v.parse().ok())
            .ok_or("no setsid worker identity")?;
        job.worker_start = i32::try_from(job.worker)
            .ok()
            .and_then(aterm_uds::process::read_process_start);
        if job.worker_start.is_none() {
            return Err(Fail::die("setsid worker has no live kernel identity"));
        }
        if field(&line, "root").as_deref() != Some(&job.root.to_string()) {
            return Err(Fail::die("exec changed the registered root PID"));
        }
        let root = i32::try_from(job.root).map_err(|e| e.to_string())?;
        let worker = i32::try_from(job.worker).map_err(|e| e.to_string())?;
        if aterm_uds::process::read_process_parent(root) != Some(std::process::id())
            || aterm_uds::process::read_process_parent(worker) != Some(job.root)
            || crate::sys::session_id(worker) != Some(worker)
            || crate::sys::session_id(root) == Some(worker)
        {
            return Err(Fail::die(
                "workload lacks the witnessed external-daemon/setsid ancestry",
            ));
        }
        Ok(job)
    }

    fn finish(&mut self) -> Result<(), Fail> {
        if let Some(mut input) = self.process.stdin.take() {
            input.write_all(b"done\n").map_err(|e| e.to_string())?;
        }
        let until = Instant::now() + HANG;
        let mut waiter = aterm_uds::deadline::ExitWait::new(&self.process);
        loop {
            if let Some(status) = self.process.try_wait().map_err(|e| e.to_string())? {
                if !status.success() {
                    return Err(Fail::die(format!("workload exit: {status}")));
                }
                break;
            }
            if Instant::now() >= until {
                return Err(Fail::die("workload exit hung"));
            }
            waiter.wait(until.saturating_duration_since(Instant::now()));
        }
        if let Some(reader) = self.reader.take() {
            reader.join().map_err(|_| "workload reader panicked")?;
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
            if let Ok(pid) = i32::try_from(self.worker)
                && self.worker_start.is_some()
                && aterm_uds::process::read_process_start(pid) == self.worker_start
            {
                // This still names our worker's exact start record.
                let _ = crate::sys::kill(pid, libc::SIGKILL);
            }
        }
    }
}

fn child() -> Res {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut worker = aterm_uds::spawnfd::command(exe)
        .args(["resource-attribution", "worker"])
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::inherit())
        .stdout(aterm_uds::spawnfd::Stdio::inherit())
        .spawn()
        .map_err(|e| e.to_string())?;
    println!("READY root={} worker={}", std::process::id(), worker.id());
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let status = worker.wait().map_err(|e| e.to_string())?;
    Ok(status.code().unwrap_or(1))
}

/// The Codex-named PTY process needs no model, credentials or network. Its
/// role here is only to let the real foreground-program observer name it.
fn observer() -> Res {
    println!("LOCAL PROGRAM OBSERVER FIXTURE");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).map_err(|e| e.to_string())?;
    Ok(0)
}

fn worker() -> Res {
    let done = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&done);
    std::thread::spawn(move || {
        let _ = std::io::stdin().read(&mut [0u8; 1]);
        stop.store(true, Ordering::Release);
    });
    let mut memory = vec![1u8; 16 * 1024 * 1024];
    let mut step = 0usize;
    while !done.load(Ordering::Acquire) {
        for byte in &mut memory {
            *byte = byte.wrapping_add(step as u8);
        }
        step = std::hint::black_box(step.wrapping_add(1));
        std::hint::black_box(&memory);
        std::thread::yield_now();
    }
    Ok(0)
}
