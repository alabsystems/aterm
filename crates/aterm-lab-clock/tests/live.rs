// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live probe tests for `aterm-lab-clock` injected via `DYLD_INSERT_LIBRARIES`.

#![cfg(target_os = "macos")]

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use std::process::ChildStdin;

use aterm_uds::spawnfd::{Child, Stdio};

const STEP_NS: i128 = 86_400 * 1_000_000_000;
const SLACK_NS: i128 = 2_000_000_000;
const HANG: Duration = Duration::from_secs(60);

const WALL: [&str; 5] = [
    "realtime",
    "realtime_nsec_np",
    "gettimeofday",
    "time",
    "systemtime",
];
const SLEEP_COUNTING: [&str; 5] = [
    "monotonic",
    "monotonic_raw",
    "monotonic_raw_approx",
    "continuous",
    "continuous_approximate",
];
const UPTIME: [&str; 3] = ["uptime_raw_approx", "absolute", "instant"];

fn library() -> &'static Path {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    LIB.get_or_init(|| {
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("aterm-lab-clock-build.noindex");
        let out = aterm_uds::spawnfd::command(env!("CARGO"))
            .args([
                "--unverified",
                "build",
                "--offline",
                "--lib",
                "-p",
                "aterm-lab-clock",
                "--manifest-path",
            ])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .stdin(Stdio::null())
            .output()
            .expect("targo runs");
        assert!(
            out.status.success(),
            "the clock library did not build:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let lib = target.join("debug/libaterm_lab_clock.dylib");
        assert!(lib.is_file(), "no {}", lib.display());
        lib
    })
}

struct Probe {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    dir: PathBuf,
}

impl Probe {
    fn start(dir: &Path, continuous: bool) -> Probe {
        std::fs::create_dir_all(dir.join("reg")).expect("record dir");
        set_offset(dir, "0");
        let mut cmd = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_lab-clock-probe"));
        cmd.env("DYLD_INSERT_LIBRARIES", library())
            .env("LABCLOCK_FILE", dir.join("offset"))
            .env("LABCLOCK_DIR", dir.join("reg"))
            .env_remove("LABCLOCK_CONTINUOUS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if continuous {
            cmd.env("LABCLOCK_CONTINUOUS", "1");
        }
        let mut child = cmd.spawn().expect("the probe starts");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("its stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Probe {
            child,
            stdin,
            lines,
            dir: dir.to_path_buf(),
        }
    }

    fn read(&mut self) -> HashMap<String, i128> {
        let stdin = self.stdin.as_mut().expect("its stdin");
        writeln!(stdin, "r")
            .and_then(|()| stdin.flush())
            .expect("ask");
        let line = self
            .lines
            .recv_timeout(HANG)
            .expect("the probe answers (hang detector)");
        line.split_whitespace()
            .filter_map(|w| {
                let (k, v) = w.split_once('=')?;
                Some((k.to_string(), v.parse().ok()?))
            })
            .collect()
    }

    fn read_stepped(&mut self, before: &HashMap<String, i128>) -> HashMap<String, i128> {
        let end = Instant::now() + HANG;
        loop {
            let now = self.read();
            if delta(before, &now, "realtime") - delta(before, &now, "uptime_raw") > STEP_NS / 2 {
                return now;
            }
            assert!(
                Instant::now() < end,
                "the probe never took the step (hang detector): {now:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn record(&self) -> String {
        std::fs::read_to_string(self.dir.join(format!("reg/{}.txt", self.child.id())))
            .unwrap_or_default()
    }

    fn end(mut self) {
        drop(self.stdin.take());
        let end = Instant::now() + HANG;
        while self.child.try_wait().expect("wait").is_none() {
            assert!(
                Instant::now() < end,
                "the probe did not end (hang detector)"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn set_offset(dir: &Path, secs: &str) {
    std::fs::write(dir.join("offset.tmp"), format!("{secs}\n")).expect("offset");
    std::fs::rename(dir.join("offset.tmp"), dir.join("offset")).expect("offset in place");
}

fn delta(a: &HashMap<String, i128>, b: &HashMap<String, i128>, k: &str) -> i128 {
    b.get(k).copied().expect(k) - a.get(k).copied().expect(k)
}

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("aterm-lab-clock-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    d
}

fn step(name: &str, continuous: bool) -> (HashMap<&'static str, i128>, i128, String) {
    let dir = scratch(name);
    let mut p = Probe::start(&dir, continuous);
    let before = p.read();
    assert!(
        p.record().contains("offset_ns=0 "),
        "the record at load: {:?}",
        p.record()
    );
    set_offset(&dir, "86400");
    let after = p.read_stepped(&before);
    let up = delta(&before, &after, "uptime_raw");
    let moved = WALL
        .iter()
        .chain(&SLEEP_COUNTING)
        .chain(&UPTIME)
        .map(|&k| (k, delta(&before, &after, k) - up))
        .collect();
    let record = p.record();
    p.end();
    let _ = std::fs::remove_dir_all(&dir);
    (moved, up, record)
}

fn assert_moved(moved: &HashMap<&str, i128>, clocks: &[&str], by: i128) {
    for k in clocks {
        let d = moved[k];
        assert!(
            (d - by).abs() <= SLACK_NS,
            "{k} moved {d} ns beyond uptime, not {by}: {moved:?}"
        );
    }
}

#[test]
fn a_step_moves_the_wall_and_the_sleep_clocks_by_the_step_and_never_uptime() {
    let (moved, up, record) = step("continuous", true);
    assert!((0..STEP_NS / 2).contains(&up), "uptime moved {up} ns");
    assert_moved(&moved, &WALL, STEP_NS);
    assert_moved(&moved, &SLEEP_COUNTING, STEP_NS);
    assert_moved(&moved, &UPTIME, 0);
    assert!(
        record.contains("offset_ns=86400000000000 continuous=1 "),
        "{record:?}"
    );
    assert!(
        record.contains("exe=") && record.contains("lab-clock-probe"),
        "{record:?}"
    );
}

#[test]
fn without_the_continuous_switch_a_step_moves_the_wall_alone() {
    let (moved, _, record) = step("wall", false);
    assert_moved(&moved, &WALL, STEP_NS);
    assert_moved(&moved, &SLEEP_COUNTING, 0);
    assert_moved(&moved, &UPTIME, 0);
    assert!(
        record.contains("offset_ns=86400000000000 continuous=0 "),
        "{record:?}"
    );
}
