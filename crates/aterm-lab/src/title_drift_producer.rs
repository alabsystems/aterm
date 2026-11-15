// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The source PTY's gated producer. It acquires no admission claim: its parent owns the window.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};
use aterm_uds::process::Identity;

use super::{CLEAR, HANG, Work, nap};

pub(super) struct Files {
    pub dir: PathBuf,
}

#[derive(Clone, Copy)]
enum Await {
    Reply,
    Gate(Identity),
}

impl Files {
    pub fn create(home: &Path, work: &Work) -> Result<Self, String> {
        let dir = home.join("title-drift");
        std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
        let files = Self { dir };
        let pid = i32::try_from(std::process::id()).map_err(|e| e.to_string())?;
        let owner = aterm_uds::process::read_identity(pid)
            .ok_or("cannot identify the title-drift driver")?;
        files.put("owner", &owner.token())?;
        files.put(
            "work",
            &format!(
                "{} {} {} {} {}",
                work.duration_ms, work.typing_ms, work.block_bytes, work.output_ms, work.key_ms
            ),
        )?;
        Ok(files)
    }

    fn put(&self, name: &str, text: &str) -> Result<(), String> {
        crate::util::atomic_write(&self.dir.join(name), text.as_bytes()).map_err(|e| e.to_string())
    }

    pub fn gate(&self, name: &str) -> Result<(), String> {
        self.put(name, "go\n")
    }

    pub fn wait(&self, name: &str) -> Result<String, String> {
        self.wait_with(
            name,
            Await::Reply,
            Instant::now,
            aterm_uds::process::read_identity,
            nap,
        )
    }

    fn wait_gate(&self, name: &str, owner: Identity) -> Result<String, String> {
        self.wait_with(
            name,
            Await::Gate(owner),
            Instant::now,
            aterm_uds::process::read_identity,
            nap,
        )
    }

    fn wait_with(
        &self,
        name: &str,
        awaiting: Await,
        mut clock: impl FnMut() -> Instant,
        mut identity: impl FnMut(i32) -> Option<Identity>,
        mut pause: impl FnMut(Duration) -> Result<(), String>,
    ) -> Result<String, String> {
        let start = clock();
        loop {
            // A ready file must not bypass an interruption already waiting for this run.
            pause(Duration::ZERO)?;
            if let Ok(why) = std::fs::read_to_string(self.dir.join("failed")) {
                return Err(format!("producer failed: {why}"));
            }
            // Setup can legitimately wait for capacity or native focus. Its driver owns
            // these gates; a reply from the producer still has the one-minute hang detector.
            if let Await::Gate(owner) = awaiting
                && identity(owner.pid) != Some(owner)
            {
                return Err(format!(
                    "title-drift driver identity unavailable or changed before {name}"
                ));
            }
            match std::fs::read_to_string(self.dir.join(name)) {
                Ok(text) => return Ok(text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
            if matches!(awaiting, Await::Reply) && clock().duration_since(start) >= HANG {
                return Err(format!("producer never reached {name}"));
            }
            pause(Duration::from_millis(if matches!(awaiting, Await::Reply) {
                2
            } else {
                10
            }))?;
        }
    }
}

fn work(files: &Files) -> Result<Work, String> {
    let text = std::fs::read_to_string(files.dir.join("work")).map_err(|e| e.to_string())?;
    let values = text
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let [duration_ms, typing_ms, block_bytes, output_ms, key_ms] = values.as_slice() else {
        return Err("invalid producer workload".into());
    };
    let work = Work {
        profile: "producer".into(),
        duration_ms: *duration_ms,
        typing_ms: *typing_ms,
        block_bytes: usize::try_from(*block_bytes).map_err(|e| e.to_string())?,
        output_ms: *output_ms,
        key_ms: *key_ms,
    };
    work.validate()?;
    Ok(work)
}

fn run(files: &Files) -> Result<(), String> {
    let work = work(files)?;
    let owner = std::fs::read_to_string(files.dir.join("owner")).map_err(|e| e.to_string())?;
    let owner = Identity::parse(&owner).ok_or("invalid title-drift driver identity")?;
    let (block, last) = work.payloads();
    let mut out = std::io::stdout().lock();
    out.write_all(b"warm source\r\n")
        .and_then(|()| out.flush())
        .map_err(|e| e.to_string())?;
    files.put("ready", &format!("pid={}\n", std::process::id()))?;
    files.wait_gate("clear", owner)?;
    out.write_all(CLEAR)
        .and_then(|()| out.flush())
        .map_err(|e| e.to_string())?;
    files.put("armed", "warm output cleared\n")?;
    files.wait_gate("start", owner)?;
    let start = Instant::now();
    let start_unix_ms = crate::util::now_ms();
    let start_monotonic_ns = crate::sys::monotonic_ns();
    let mut due_ns = start_monotonic_ns;
    let mut sent_ns = Vec::with_capacity(usize::try_from(work.blocks()).unwrap_or(0));
    let mut bytes = 0u64;
    let gap_ns = work.output_ms * 1_000_000;
    for i in 0..work.blocks() {
        loop {
            let now = crate::sys::monotonic_ns();
            if now >= due_ns {
                break;
            }
            nap(Duration::from_nanos(due_ns - now))?;
        }
        let sent_at_ns = crate::sys::monotonic_ns();
        let payload = if i + 1 == work.blocks() {
            &last
        } else {
            &block
        };
        out.write_all(payload)
            .and_then(|()| out.flush())
            .map_err(|e| e.to_string())?;
        bytes += u64::try_from(payload.len()).map_err(|e| e.to_string())?;
        sent_ns.push(sent_at_ns);
        if i == 0 {
            files.put(
                "started.json",
                &json!({"first_output_monotonic_ns": sent_at_ns}).to_string(),
            )?;
        }
        // A slow write delays later output; it never compresses the next gap to catch up.
        due_ns = sent_at_ns + gap_ns;
    }
    if let Some(wait) =
        (start + Duration::from_millis(work.duration_ms)).checked_duration_since(Instant::now())
    {
        nap(wait)?;
    }
    let done = json!({
        "version": 1, "pid": std::process::id(), "blocks": work.blocks(), "bytes": bytes,
        "clock": "host_CLOCK_MONOTONIC_ns", "start_unix_ms": start_unix_ms,
        "start_monotonic_ns": start_monotonic_ns,
        "end_monotonic_ns": crate::sys::monotonic_ns(),
        "elapsed_ns": u64::try_from(start.elapsed().as_nanos()).map_err(|e| e.to_string())?,
        "sent_ns": sent_ns,
    });
    files.put("done.json", &done.to_string())?;
    drop(out);
    // Keep the source session alive until its owning Instance closes the PTY.
    let mut input = std::io::stdin().lock();
    let mut discard = [0u8; 1024];
    while input.read(&mut discard).map_err(|e| e.to_string())? != 0 {}
    Ok(())
}

/// Private source PTY program; the parent opens the timing gate and owns admission.
pub(crate) fn main(args: &[String]) -> i32 {
    let [dir] = args else {
        eprintln!("producer requires its private directory");
        return 2;
    };
    let dir = PathBuf::from(dir);
    if !dir.is_absolute() {
        eprintln!("producer directory must be absolute");
        return 2;
    }
    crate::stop::install();
    let files = Files { dir };
    match run(&files) {
        Ok(()) => 0,
        Err(why) => {
            let _ = files.put("failed", &why);
            eprintln!("title-drift producer: {why}");
            3
        }
    }
}

pub(super) fn validate(text: &str, work: &Work) -> Result<(u64, u64), String> {
    let value: Value = aterm_json::from_str(text).map_err(|e| e.to_string())?;
    let n = |key: &str| value.get(key).and_then(Value::as_u64);
    let sent = value
        .get("sent_ns")
        .and_then(Value::as_array)
        .ok_or("producer send times missing")?;
    if n("version") != Some(1)
        || n("blocks") != Some(work.blocks())
        || n("bytes") != Some(work.blocks() * u64::try_from(work.block_bytes).unwrap_or(0))
        || n("elapsed_ns").is_none_or(|n| n < work.duration_ms * 1_000_000)
        || sent.len() != usize::try_from(work.blocks()).unwrap_or(0)
    {
        return Err("producer's workload was incomplete".into());
    }
    let times = sent
        .iter()
        .map(|v| v.as_u64().ok_or("invalid producer timestamp"))
        .collect::<Result<Vec<_>, _>>()?;
    if times.windows(2).any(|pair| {
        pair[1]
            .checked_sub(pair[0])
            .is_none_or(|gap| gap < work.output_ms * 1_000_000)
    }) {
        return Err("producer compressed or reordered output sends".into());
    }
    let start = n("start_monotonic_ns")
        .filter(|n| *n > 0)
        .ok_or("producer start clock missing")?;
    let end = n("end_monotonic_ns")
        .filter(|n| *n > start)
        .ok_or("producer end clock missing")?;
    if value.get("clock").and_then(Value::as_str) != Some("host_CLOCK_MONOTONIC_ns")
        || times.first().is_none_or(|first| *first < start)
        || times.last().is_none_or(|last| *last > end)
    {
        return Err("producer timestamp exceeds its duration".into());
    }
    Ok((
        *times.first().ok_or("no output sends")?,
        *times.last().ok_or("no output sends")?,
    ))
}

#[cfg(test)]
#[path = "title_drift_producer_tests.rs"]
mod tests;
