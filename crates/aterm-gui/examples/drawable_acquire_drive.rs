// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A visible-window regression for a blocked native drawable acquisition.
//!
//! Run the control-conformance example with `--ctl /absolute/path/to/aterm`
//! and optionally `--output /new/receipt/directory`. It launches only its own
//! children, each running the SHIPPING GUI entry and a private raw PTY program.
//! After a successful native present, a compile-time-only hook holds one actual
//! acquisition for 750 ms. `dims` replies require App::user_event to execute;
//! control-thread metrics alone would not prove main-loop responsiveness.
//!
//! The asynchronous child must answer six reads within 100 ms while acquisition
//! remains blocked, accept a resize and newer PTY output, then present and capture
//! the new green raster. The synchronous child crosses the identical hook on the
//! main thread and MUST miss that budget. A separately scheduled driver clock
//! records lateness; an overloaded driver is reported as an inconclusive run,
//! never evidence that the renderer passed. This is injected selector latency,
//! not a claim to have exhausted Core Animation's compositor pool naturally.
//! A third child closes the held native window while another private window
//! continues answering reads, then checks survivor pixels after late completion.
//!
//! Exit 0 = both positive and negative controls passed; 1 = finding;
//! 2 = not run / unreliable driver. Artifacts survive every outcome.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("drawable-acquire-drive: NOT RUN — requires native macOS Metal");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() {
    std::process::exit(macos::run());
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fs::{self, File};
    use std::io::{Cursor, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use aterm_gpu::{AcquireProbe, install_acquire_probe};

    const HOLD: Duration = Duration::from_millis(750);
    // Allow the extra native-window initialization in the retirement phase.
    const CLOSE_HOLD: Duration = Duration::from_secs(3);
    const RESPONSE_BUDGET: Duration = Duration::from_millis(100);
    const SAMPLE_GAP: Duration = Duration::from_millis(80);
    const CLOCK_GAP: Duration = Duration::from_millis(25);
    const DRIVER_SLACK: Duration = Duration::from_millis(25);
    const STARTUP_BUDGET: Duration = Duration::from_secs(30);
    const CHILD_BUDGET: Duration = Duration::from_secs(75);

    type Result<T> = std::result::Result<T, String>;

    const PROGRAM: &str = r#"import os, sys, tty
assert all(s.isatty() for s in (sys.stdin, sys.stdout, sys.stderr))
tty.setraw(0)
def paint(green):
    color = '16;224;48' if green else '16;48;224'
    label = 'LATEST GREEN' if green else 'BASELINE BLUE'
    data = '\x1b[0m\x1b[2J\x1b[H' + label
    for row in range(3, 9):
        data += '\x1b[%d;4H\x1b[48;2;%sm%s\x1b[0m' % (row, color, ' ' * 24)
    data += '\x1b[11;1HREADY> '
    os.write(1, data.encode())
paint(False)
while True:
    data = os.read(0, 256)
    if not data:
        break
    if b'b' in data:
        paint(True)
    elif b'a' in data:
        paint(False)
"#;

    fn option(args: &[String], name: &str) -> Option<String> {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
    }

    pub fn run() -> i32 {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if let Some(mode) = option(&args, "--child") {
            if !matches!(mode.as_str(), "async" | "sync" | "async-close") {
                return 2;
            }
            return child(&args, mode == "sync", mode == "async-close");
        }
        match parent(&args) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("drawable-acquire-drive: {error}");
                if error.starts_with("NOT RUN") { 2 } else { 1 }
            }
        }
    }

    fn parent(args: &[String]) -> Result<()> {
        let ctl = option(args, "--ctl").ok_or(
            "NOT RUN — usage: drawable_acquire_drive --ctl /path/to/aterm [--output NEW_DIR]",
        )?;
        let ctl = fs::canonicalize(ctl).map_err(|e| format!("NOT RUN — controller: {e}"))?;
        let own = std::env::current_exe().map_err(|e| e.to_string())?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis();
        let root = option(args, "--output").map_or_else(
            || std::env::temp_dir().join(format!("aterm-drawable-{}-{stamp}", std::process::id())),
            PathBuf::from,
        );
        // A new directory prevents an old successful receipt being mistaken for
        // the result of this run, and confines every spawned session and artifact.
        fs::create_dir(&root).map_err(|e| format!("NOT RUN — new output directory: {e}"))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
        println!("drawable-acquire-drive: receipts {}", root.display());

        for mode in ["async", "sync", "async-close"] {
            let phase = root.join(mode);
            for dir in [
                phase.clone(),
                phase.join("cfg"),
                phase.join("cfg/aterm"),
                phase.join("state"),
                phase.join("run"),
            ] {
                fs::create_dir(&dir).map_err(|e| e.to_string())?;
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
                    .map_err(|e| e.to_string())?;
            }
            fs::write(phase.join("program.py"), PROGRAM).map_err(|e| e.to_string())?;
            let program_path =
                aterm_json::to_string(&phase.join("program.py").display().to_string())
                    .map_err(|e| e.to_string())?;
            fs::write(
                phase.join("cfg/aterm/aterm.toml"),
                format!(
                    concat!(
                        "shell = \"/usr/bin/python3\"\nshell_args = [\"-S\", \"-u\", {}]\n",
                        "restore_session = false\nfont_px = 20\n",
                        "cursor_trail = false\nload_adaptive_motion = false\n",
                        "[packages]\nenabled = false\n",
                    ),
                    program_path
                ),
            )
            .map_err(|e| e.to_string())?;
            let log = File::create(phase.join("host.log")).map_err(|e| e.to_string())?;
            let mut command = Command::new(&own);
            command
                .args(["--child", mode, "--ctl"])
                .arg(&ctl)
                .arg("--root")
                .arg(&phase);
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("ATERM_") || key == "TERM_PROGRAM" {
                    command.env_remove(key);
                }
            }
            command
                .env("ATERM_CONTROL_SOCK", phase.join("ctl.sock"))
                .env("XDG_RUNTIME_DIR", phase.join("run"))
                .env("XDG_CONFIG_HOME", phase.join("cfg"))
                .env("XDG_STATE_HOME", phase.join("state"))
                .env("ATERM_STATE_HOME", phase.join("state"))
                .env("ATERM_LINES", "22")
                .env("ATERM_COLUMNS", "76")
                .env("SHELL", "/bin/sh")
                .env("ENV", "")
                .current_dir(&phase)
                .stdin(Stdio::null())
                .stdout(log.try_clone().map_err(|e| e.to_string())?)
                .stderr(log)
                .process_group(0);
            let mut child = command.spawn().map_err(|e| e.to_string())?;
            let deadline = Instant::now() + CHILD_BUDGET;
            let status = loop {
                if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                    break Some(status);
                }
                if Instant::now() >= deadline {
                    break None;
                }
                thread::sleep(Duration::from_millis(20));
            };
            // Only the new process group established above, never a discovered
            // user/peer process. This also reaps the private PTY on failed drives.
            // SAFETY: kill accepts the child-owned process group id; SIGTERM is a
            // teardown notification, and ESRCH just means everything exited.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGTERM);
            }
            if status.is_none() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "FAIL — {mode} exceeded child watchdog; see {}",
                    phase.display()
                ));
            }
            let status = status.expect("checked watchdog");
            if !status.success() {
                let prefix = if status.code() == Some(2) {
                    "NOT RUN"
                } else {
                    "FAIL"
                };
                return Err(format!(
                    "{prefix} — {mode} {status}; see {}",
                    phase.display()
                ));
            }
            // main_entry intentionally process::exit(0)s on normal GUI exit;
            // it never returns to join the driver. A zero status without the
            // driver's completed assertions must therefore fail closed.
            if fs::read_to_string(phase.join("checks-complete"))
                .ok()
                .as_deref()
                != Some(mode)
            {
                return Err(format!(
                    "FAIL — {mode} exited before completing its assertions; see {}",
                    phase.display()
                ));
            }
            println!("  ok {mode}; {}", phase.join("receipt.txt").display());
        }
        println!(
            "drawable-acquire-drive: PASS — responsive worker, caught synchronous control, and nonblocking native-window retirement"
        );
        Ok(())
    }

    fn child(args: &[String], synchronous: bool, close_while_blocked: bool) -> i32 {
        let Some(root) = option(args, "--root").map(PathBuf::from) else {
            return 2;
        };
        let Some(ctl) = option(args, "--ctl").map(PathBuf::from) else {
            return 2;
        };
        let probe = AcquireProbe::new(synchronous);
        if !install_acquire_probe(Arc::clone(&probe)) {
            eprintln!("FAIL — acquisition probe already installed");
            return 1;
        }
        let program = root.join("program.py");
        let driver = thread::spawn(move || {
            let result = Driver::new(root, ctl, probe.clone(), synchronous, close_while_blocked)
                .and_then(Driver::run);
            probe.release();
            match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("drawable-acquire-drive: {error}");
                    // Error cleanup cannot require a functioning GUI. The parent
                    // owns the process group and cleans the PTY even on this exit.
                    std::process::exit(if error.starts_with("NOT RUN") { 2 } else { 1 });
                }
            }
        });
        // Like src/main.rs, this callable receives argv[1..], with no argv0.
        aterm_gui::main_entry(vec![
            "-e".into(),
            "/usr/bin/python3".into(),
            "-S".into(),
            "-u".into(),
            program.into_os_string(),
        ]);
        driver.join().unwrap_or(1)
    }

    struct Driver {
        root: PathBuf,
        ctl: PathBuf,
        probe: Arc<AcquireProbe>,
        synchronous: bool,
        close_while_blocked: bool,
        session: String,
        receipt: File,
    }

    impl Driver {
        fn new(
            root: PathBuf,
            ctl: PathBuf,
            probe: Arc<AcquireProbe>,
            synchronous: bool,
            close_while_blocked: bool,
        ) -> Result<Self> {
            let receipt = File::create(root.join("receipt.txt")).map_err(|e| e.to_string())?;
            Ok(Self {
                root,
                ctl,
                probe,
                synchronous,
                close_while_blocked,
                session: String::new(),
                receipt,
            })
        }

        fn record(&mut self, line: impl AsRef<str>) -> Result<()> {
            writeln!(self.receipt, "{}", line.as_ref()).map_err(|e| e.to_string())?;
            self.receipt.flush().map_err(|e| e.to_string())
        }

        fn ctl(&self, args: &[&str]) -> Result<String> {
            control(&self.ctl, &self.root.join("ctl.sock"), args)
        }

        fn own_ctl(&self, args: &[&str]) -> Result<String> {
            let mut scoped = vec![self.session.as_str()];
            scoped.extend_from_slice(args);
            self.ctl(&scoped)
        }

        fn wait_for(&self, what: &str, mut predicate: impl FnMut() -> Result<bool>) -> Result<()> {
            let deadline = Instant::now() + STARTUP_BUDGET;
            while Instant::now() < deadline {
                if predicate()? {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(40));
            }
            Err(format!("FAIL — timed out waiting for {what}"))
        }

        fn run(mut self) -> Result<()> {
            self.record(format!(
                "mode={} pid={} hold_ms={} response_budget_ms={}",
                if self.close_while_blocked {
                    "async-close"
                } else if self.synchronous {
                    "sync-control"
                } else {
                    "worker"
                },
                std::process::id(),
                if self.close_while_blocked {
                    CLOSE_HOLD
                } else {
                    HOLD
                }
                .as_millis(),
                RESPONSE_BUDGET.as_millis()
            ))?;
            self.wait_for("private socket", || Ok(self.root.join("ctl.sock").exists()))?;
            self.wait_for("private seed session", || {
                Ok(self
                    .ctl(&["sessions"])?
                    .lines()
                    .any(|line| line.split_whitespace().any(|part| part == "alive")))
            })?;
            let sessions = self.ctl(&["sessions"])?;
            let ids = sessions
                .lines()
                .filter(|line| line.split_whitespace().any(|part| part == "alive"))
                .filter_map(|line| line.split_whitespace().find(|part| part.starts_with("s-")))
                .collect::<Vec<_>>();
            if ids.len() != 1 {
                return Err(format!(
                    "FAIL — private instance must own exactly one session: {sessions}"
                ));
            }
            self.session = format!("@{}", ids[0]);
            self.record(self.ctl(&["version"])?)?;
            self.record(self.own_ctl(&["status"])?)?;
            self.wait_for("private raw PTY baseline", || {
                Ok(self.own_ctl(&["text"])?.contains("BASELINE BLUE"))
            })?;
            self.wait_for("initial successful native present", || {
                let metrics = self.ctl(&["metrics"])?;
                Ok(metrics.contains("backend=gpu")
                    && field_u64(&metrics, "frames").is_some_and(|frames| frames > 0))
            })?;
            let baseline_dims = self.own_ctl(&["dims"])?;
            if !baseline_dims.contains("visible_viewers=1") || baseline_dims.contains("window=none")
            {
                return Err(format!(
                    "NOT RUN — fixture needs a visible attached window: {baseline_dims}"
                ));
            }
            self.record(format!("baseline_dims {baseline_dims}"))?;
            let baseline = self.capture("baseline.png")?;
            if baseline.blue < 2000 || baseline.green != 0 {
                return Err(format!(
                    "FAIL — baseline raster is not the private blue scene: {baseline:?}"
                ));
            }
            let baseline_window = self.capture_window("baseline-window.png")?;
            // Window chrome includes macOS's green traffic light. The authored
            // 24x6-cell block must dominate that small unrelated native control;
            // application-only captures above/below retain the strict zero test.
            if baseline_window.blue < 2000 || baseline_window.green >= 2000 {
                return Err(format!(
                    "FAIL — baseline submitted window lacks private blue pixels: {baseline_window:?}"
                ));
            }
            if self.close_while_blocked {
                return self.close_during_acquire();
            }
            if !self.probe.arm() {
                return Err("FAIL — acquisition gate did not arm".into());
            }
            // This is a real write to our raw PTY, which causes the production
            // redraw path to acquire. Existing pending baseline work is allowed:
            // the entered latch, not a guessed frame ordinal, defines the hold.
            // A previously queued redraw can enter the hold before the key's
            // main-thread acknowledgment. Submit on a separate driver thread so
            // the synchronous negative still gets an independent release.
            let trigger_ctl = self.ctl.clone();
            let trigger_socket = self.root.join("ctl.sock");
            let trigger_session = self.session.clone();
            let trigger = thread::spawn(move || {
                control(
                    &trigger_ctl,
                    &trigger_socket,
                    &[&trigger_session, "key", "a"],
                )
            });
            if !self.probe.wait_entered(Duration::from_secs(3)) || !self.probe.blocked() {
                return Err("FAIL — native acquisition never entered the armed hold".into());
            }
            let entered = Instant::now();
            let release_probe = Arc::clone(&self.probe);
            let releaser = thread::spawn(move || {
                thread::sleep(HOLD);
                let at = Instant::now();
                release_probe.release();
                at
            });
            let clock_probe = Arc::clone(&self.probe);
            let clock = thread::spawn(move || driver_clock(&clock_probe, entered));
            let mut samples = Vec::new();
            for index in 0..6 {
                let target = entered + SAMPLE_GAP * index;
                thread::sleep(target.saturating_duration_since(Instant::now()));
                let before = self.probe.blocked();
                let start = Instant::now();
                let dims = self.own_ctl(&["dims"])?;
                let elapsed = start.elapsed();
                let after = self.probe.blocked();
                self.record(format!("heartbeat index={index} start_ms={:.3} late_ms={:.3} roundtrip_ms={:.3} blocked_before={before} blocked_after={after} valid_dims={}", start.duration_since(entered).as_secs_f64()*1000.0, start.saturating_duration_since(target).as_secs_f64()*1000.0, elapsed.as_secs_f64()*1000.0, dims.contains("session=")))?;
                samples.push((before, after, elapsed, dims.contains("session=")));
                if self.synchronous {
                    break;
                }
                if index == 0 {
                    self.own_ctl(&["key", "b"])?;
                    // The OS-window form drives actual native resize and stale
                    // drawable generation handling, unlike grid-only resizing.
                    self.own_ctl(&["resize", "px", "1180", "780"])?;
                    self.record(format!(
                        "resize_and_new_output_while_blocked={}",
                        self.probe.blocked()
                    ))?;
                    if !self.probe.blocked() {
                        return Err("FAIL — resize/output missed the blocked interval".into());
                    }
                }
            }
            // Read this after the held interval's last heartbeat. An unrelated
            // baseline frame that completed just before entry cannot satisfy the
            // requirement for a successful frame after the blocked attempt.
            let frames_held =
                field_u64(&self.ctl(&["metrics"])?, "frames").ok_or("FAIL — frames absent")?;
            let released = releaser
                .join()
                .map_err(|_| "FAIL — release thread panicked")?;
            trigger
                .join()
                .map_err(|_| "FAIL — trigger thread panicked")??;
            let clock = clock.join().map_err(|_| "FAIL — driver clock panicked")?;
            let max_lateness = clock
                .iter()
                .map(|(_, late)| *late)
                .max()
                .unwrap_or(Duration::MAX);
            for (at, late) in &clock {
                self.record(format!(
                    "driver_clock at_ms={:.3} late_ms={:.3}",
                    at.as_secs_f64() * 1000.0,
                    late.as_secs_f64() * 1000.0
                ))?;
            }
            self.record(format!("release_ms={:.3} entries={} timed_out={} driver_samples={} max_driver_lateness_ms={:.3}", released.duration_since(entered).as_secs_f64()*1000.0, self.probe.entries(), self.probe.timed_out(), clock.len(), max_lateness.as_secs_f64()*1000.0))?;
            if self.probe.entries() != 1 || self.probe.timed_out() {
                return Err(
                    "FAIL — acquisition hold did not have exactly one deliberate entry/release"
                        .into(),
                );
            }
            if clock.len() < 20 || max_lateness > DRIVER_SLACK {
                return Err(
                    "NOT RUN — independent driver clock missed its 25 ms calibration".into(),
                );
            }
            if self.synchronous {
                let (before, after, elapsed, valid) = samples[0];
                if !before || after || !valid || elapsed < Duration::from_millis(500) {
                    return Err("FAIL — historical synchronous acquisition did not reproduce main-loop starvation".into());
                }
                self.own_ctl(&["key", "b"])?;
                self.own_ctl(&["resize", "px", "1180", "780"])?;
                self.record("negative_control=caught (same selector gate blocked the main loop)")?;
            } else if samples.len() != 6
                || samples.iter().any(|&(before, after, elapsed, valid)| {
                    !before || !after || !valid || elapsed > RESPONSE_BUDGET
                })
            {
                return Err(
                    "FAIL — main loop was not responsive throughout the held acquisition".into(),
                );
            }
            self.wait_for("newer PTY scene", || {
                Ok(self.own_ctl(&["text"])?.contains("LATEST GREEN"))
            })?;
            self.wait_for("successful present after releasing acquisition", || {
                Ok(field_u64(&self.ctl(&["metrics"])?, "frames").is_some_and(|n| n > frames_held))
            })?;
            self.wait_for("resized native surface", || {
                let dims = self.own_ctl(&["dims"])?;
                Ok(field_u64(&dims, "surface_w") == Some(1180)
                    && field_u64(&dims, "surface_h") == Some(780))
            })?;
            self.record(format!("final_dims {}", self.own_ctl(&["dims"])?))?;
            let latest = self.capture("latest-resized.png")?;
            if latest.green < 2000
                || latest.blue != 0
                || (latest.width, latest.height) == (baseline.width, baseline.height)
            {
                return Err(format!(
                    "FAIL — latest resized capture retained stale pixels/geometry: {latest:?}"
                ));
            }
            let latest_window = self.capture_window("latest-resized-window.png")?;
            if latest_window.green < 2000
                || latest_window.blue >= 2000
                || (latest_window.width, latest_window.height)
                    == (baseline_window.width, baseline_window.height)
            {
                return Err(format!(
                    "FAIL — submitted window retained stale pixels/geometry: {latest_window:?}"
                ));
            }
            self.record(format!("final_metrics {}", self.ctl(&["metrics"])?))?;
            self.record("PASS — native present resumed; application and exact submitted-window captures contain latest green pixels and resized geometry; requesting private-session close")?;
            self.mark_checks_complete()?;
            self.own_ctl(&["close"])?;
            Ok(())
        }

        fn capture(&mut self, name: &str) -> Result<Raster> {
            let reply = self.own_ctl(&["image", "--bytes", "--meta"])?;
            let metadata = reply
                .lines()
                .find(|line| line.starts_with("image-meta "))
                .ok_or("FAIL — image metadata absent")?;
            self.record(format!("capture={name} {metadata}"))?;
            // The image is an application-render artifact, not a WindowServer
            // photograph. Its metadata is retained so this distinction survives.
            let row = reply
                .lines()
                .find(|line| {
                    line.split_whitespace()
                        .next()
                        .is_some_and(|word| word.parse::<u32>().is_ok())
                })
                .ok_or("FAIL — image payload absent")?;
            let words = row.split_whitespace().collect::<Vec<_>>();
            if words.len() != 4 {
                return Err("FAIL — unexpected image payload framing".into());
            }
            let png = aterm_codec::base64::decode_strict(words[3].as_bytes())
                .map_err(|e| e.to_string())?;
            if words[2].parse::<usize>().ok() != Some(png.len()) {
                return Err("FAIL — PNG length mismatch".into());
            }
            fs::write(self.root.join(name), &png).map_err(|e| e.to_string())?;
            self.decode_raster(name, png)
        }

        fn close_during_acquire(mut self) -> Result<()> {
            let first_session = self.session.clone();
            let first_dims = self.own_ctl(&["dims"])?;
            let first_window =
                field_u64(&first_dims, "window").ok_or("FAIL — first window id absent")?;
            if !self.probe.arm() {
                return Err("FAIL — close acquisition gate did not arm".into());
            }
            let trigger_ctl = self.ctl.clone();
            let trigger_socket = self.root.join("ctl.sock");
            let trigger_session = first_session.clone();
            let trigger = thread::spawn(move || {
                control(
                    &trigger_ctl,
                    &trigger_socket,
                    &[&trigger_session, "key", "a"],
                )
            });
            if !self.probe.wait_entered(Duration::from_secs(3)) || !self.probe.blocked() {
                return Err(
                    "FAIL — close phase did not hold the first window's acquisition".into(),
                );
            }
            let entered = Instant::now();
            let release_probe = Arc::clone(&self.probe);
            let releaser = thread::spawn(move || {
                thread::sleep(CLOSE_HOLD);
                let at = Instant::now();
                release_probe.release();
                at
            });
            let clock_probe = Arc::clone(&self.probe);
            let clock = thread::spawn(move || driver_clock(&clock_probe, entered));
            // No second surface existed at arm/entry. Thus a global one-shot
            // hook cannot accidentally hold the survivor's layer instead.
            let of = format!("of={}", first_session.trim_start_matches('@'));
            let spawned = self.ctl(&["spawn", "connected=controller", "place=window", &of])?;
            let second = spawned
                .split_whitespace()
                .find(|part| part.starts_with("s-"))
                .ok_or("FAIL — connected spawn did not identify its new session")?;
            self.session = format!("@{second}");
            if self.session == first_session {
                return Err("FAIL — spawn reused the first identity".into());
            }
            self.record(format!("survivor_status {}", self.own_ctl(&["status"])?))?;
            let second_dims = self.own_ctl(&["dims"])?;
            let second_window =
                field_u64(&second_dims, "window").ok_or("FAIL — survivor window id absent")?;
            if second_window == first_window || !second_dims.contains("visible_viewers=1") {
                return Err(format!(
                    "FAIL — survivor must occupy a second visible window: {second_dims}"
                ));
            }
            // Ensure the trigger has finished delivering before retiring its PTY;
            // otherwise a correct close could race the driver-owned pending key.
            trigger
                .join()
                .map_err(|_| "FAIL — close trigger panicked")??;
            let before = self.probe.blocked();
            let start = Instant::now();
            let closed = self.ctl(&[&first_session, "close"])?;
            let elapsed = start.elapsed();
            let after = self.probe.blocked();
            self.record(format!("close first={first_session} window={first_window} elapsed_ms={:.3} blocked_before={before} blocked_after={after} reply={}", elapsed.as_secs_f64()*1000.0, closed.trim()))?;
            if !before || !after || elapsed > RESPONSE_BUDGET || !closed.contains("closed") {
                return Err("FAIL — retiring the held native window waited for its acquire".into());
            }
            let heartbeat_start = Instant::now();
            let mut heartbeats_ok = true;
            for index in 0..6 {
                let target = heartbeat_start + SAMPLE_GAP * index;
                thread::sleep(target.saturating_duration_since(Instant::now()));
                let before = self.probe.blocked();
                let start = Instant::now();
                let dims = self.own_ctl(&["dims"])?;
                let elapsed = start.elapsed();
                let after = self.probe.blocked();
                let valid = field_u64(&dims, "window") == Some(second_window);
                self.record(format!("survivor_heartbeat index={index} at_ms={:.3} roundtrip_ms={:.3} blocked_before={before} blocked_after={after} same_window={valid}", start.duration_since(entered).as_secs_f64()*1000.0, elapsed.as_secs_f64()*1000.0))?;
                heartbeats_ok &= before && after && valid && elapsed <= RESPONSE_BUDGET;
            }
            let roster = self.ctl(&["sessions"])?;
            let live = roster
                .lines()
                .filter(|line| line.split_whitespace().any(|part| part == "alive"))
                .filter_map(|line| line.split_whitespace().find(|part| part.starts_with("s-")))
                .collect::<Vec<_>>();
            if live != [self.session.trim_start_matches('@')] {
                return Err("FAIL — native close did not retire exactly the first session".into());
            }
            let released = releaser
                .join()
                .map_err(|_| "FAIL — close releaser panicked")?;
            let clock = clock
                .join()
                .map_err(|_| "FAIL — close driver clock panicked")?;
            let max_lateness = clock
                .iter()
                .map(|(_, late)| *late)
                .max()
                .unwrap_or(Duration::MAX);
            for (at, late) in &clock {
                self.record(format!(
                    "driver_clock at_ms={:.3} late_ms={:.3}",
                    at.as_secs_f64() * 1000.0,
                    late.as_secs_f64() * 1000.0
                ))?;
            }
            self.record(format!("close_release_ms={:.3} entries={} timed_out={} driver_samples={} max_driver_lateness_ms={:.3}", released.duration_since(entered).as_secs_f64()*1000.0, self.probe.entries(), self.probe.timed_out(), clock.len(), max_lateness.as_secs_f64()*1000.0))?;
            if self.probe.entries() != 1 || self.probe.timed_out() {
                return Err(
                    "FAIL — closed layer's acquire was not independently released exactly once"
                        .into(),
                );
            }
            if clock.len() < 80 || max_lateness > DRIVER_SLACK {
                return Err(
                    "NOT RUN — close-phase driver clock missed its 25 ms calibration".into(),
                );
            }
            if !heartbeats_ok {
                return Err("FAIL — surviving window stalled behind retired acquire".into());
            }
            self.own_ctl(&["key", "b"])?;
            self.wait_for("surviving private PTY", || {
                Ok(self.own_ctl(&["text"])?.contains("LATEST GREEN"))
            })?;
            let latest = self.capture("survivor-after-release.png")?;
            let window = self.capture_window("survivor-after-release-window.png")?;
            if latest.green < 2000 || latest.blue != 0 || window.green < 2000 || window.blue >= 2000
            {
                return Err("FAIL — surviving window did not present its new green scene after late release".into());
            }
            self.record("PASS — held native layer retired without joining; surviving window answered while held and presented after late completion; requesting private-survivor close")?;
            self.mark_checks_complete()?;
            self.own_ctl(&["close"])?;
            Ok(())
        }

        fn mark_checks_complete(&self) -> Result<()> {
            let mode = if self.close_while_blocked {
                "async-close"
            } else if self.synchronous {
                "sync"
            } else {
                "async"
            };
            fs::write(self.root.join("checks-complete"), mode).map_err(|e| e.to_string())
        }

        fn capture_window(&mut self, name: &str) -> Result<Raster> {
            let reply = self.ctl(&["window", "front", name])?;
            self.record(format!("submitted_window={name} {}", reply.trim()))?;
            // Server confinement puts this single filename below the PRIVATE
            // socket directory. The shipping CLI completes the artifact ACK.
            let png = fs::read(self.root.join("images").join(name))
                .map_err(|e| format!("FAIL — submitted window PNG: {e}"))?;
            self.decode_raster(name, png)
        }

        fn decode_raster(&mut self, name: &str, png: Vec<u8>) -> Result<Raster> {
            let mut reader = aterm_png::Decoder::new(Cursor::new(png))
                .read_info()
                .map_err(|e| e.to_string())?;
            let mut pixels = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut pixels).map_err(|e| e.to_string())?;
            let stride = match info.color_type {
                aterm_png::ColorType::Rgb => 3,
                aterm_png::ColorType::Rgba => 4,
                other => return Err(format!("FAIL — unexpected captured pixel layout {other:?}")),
            };
            let mut raster = Raster {
                width: info.width,
                height: info.height,
                blue: 0,
                green: 0,
            };
            for pixel in pixels[..info.buffer_size()].chunks_exact(stride) {
                if pixel[0] < 60 && pixel[1] < 100 && pixel[2] > 180 {
                    raster.blue += 1;
                }
                if pixel[0] < 80 && pixel[1] > 170 && pixel[2] < 100 {
                    raster.green += 1;
                }
            }
            self.record(format!("raster={name} {raster:?}"))?;
            Ok(raster)
        }
    }

    #[derive(Debug)]
    struct Raster {
        width: u32,
        height: u32,
        blue: usize,
        green: usize,
    }

    fn field_u64(text: &str, name: &str) -> Option<u64> {
        text.split_whitespace()
            .filter_map(|part| part.split_once('='))
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value.parse().ok())
    }

    fn driver_clock(probe: &AcquireProbe, entered: Instant) -> Vec<(Duration, Duration)> {
        let mut next = entered + CLOCK_GAP;
        let mut samples = Vec::new();
        while probe.blocked() {
            thread::sleep(next.saturating_duration_since(Instant::now()));
            let now = Instant::now();
            samples.push((
                now.duration_since(entered),
                now.saturating_duration_since(next),
            ));
            next += CLOCK_GAP;
        }
        samples
    }

    fn control(ctl: &Path, socket: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new(ctl)
            .arg("ctl")
            .arg("--sock")
            .arg(socket)
            .args(["--timeout", "3"])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
        if !output.status.success() || text.starts_with("ERR ") {
            return Err(format!(
                "FAIL — ctl {args:?}: {} {}",
                text.chars().take(250).collect::<String>(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(text)
    }
}
