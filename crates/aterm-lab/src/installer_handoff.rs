// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live same-image replacements with active installer custody or a sessionless native window.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::instance::{self, Instance, Launch, World};
use crate::{Fail, Res};

const HANG: Duration = Duration::from_secs(60);

struct ProbeImage {
    path: PathBuf,
    sha256: String,
}

impl ProbeImage {
    fn read(path: &Path) -> Result<Self, String> {
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        let sha256 = aterm_update_core::sha256_file(&path)?;
        Ok(Self { path, sha256 })
    }

    fn record(&self, out: &Path) -> Result<(), String> {
        write_new_report(
            &out.join("image.json"),
            &aterm_json::json!({
                "schema": 1, "image_path": self.path.to_string_lossy().into_owned(),
                "image_sha256": self.sha256,
            }),
        )
        .map_err(|e| e.to_string())
    }

    fn recheck(&self) -> Result<(), String> {
        if aterm_update_core::sha256_file(&self.path)? != self.sha256 {
            return Err("the requested image changed during the handoff probe".into());
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Custody {
    sid: String,
    child: i32,
    birth: u64,
}

fn record(text: &str, stage: &str, gui: i32) -> Option<Custody> {
    text.lines().rev().find_map(|line| {
        let (_, fields) = line.split_once("aterm-lab-installer ")?;
        let field = |name: &str| {
            fields
                .split_whitespace()
                .find_map(|word| word.strip_prefix(name))
        };
        if field("stage=") != Some(stage)
            || field("active=") != Some("true")
            || field("gui=").and_then(|value| value.parse::<i32>().ok()) != Some(gui)
        {
            return None;
        }
        let sid = field("sid=")?;
        let child = field("child=")?.parse().ok()?;
        let birth = field("birth=")?.parse().ok()?;
        (crate::stub::first_sid(sid) == Some(sid) && child > 1 && birth > 0).then(|| Custody {
            sid: sid.into(),
            child,
            birth,
        })
    })
}

fn wait<T>(what: &str, mut read: impl FnMut() -> Option<T>) -> Result<T, String> {
    let deadline = Instant::now() + HANG;
    loop {
        if let Some(value) = read() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!("one-minute hang detector: {what}"));
        }
        crate::stop::sleep(Duration::from_millis(50)).map_err(|e| format!("{e:?}"))?;
    }
}

fn server_pid(socket: &Path) -> Option<i32> {
    let stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    aterm_uds::fdpass::peer_pid(&stream)
        .ok()
        .and_then(|pid| i32::try_from(pid).ok())
}

struct OwnedWindow {
    socket: PathBuf,
    last: aterm_uds::process::Identity,
    armed: bool,
}

impl OwnedWindow {
    fn take(socket: &Path) -> Result<Self, String> {
        let last = server_pid(socket)
            .and_then(aterm_uds::process::read_identity)
            .ok_or("private GUI identity unavailable")?;
        Ok(Self {
            socket: socket.into(),
            last,
            armed: true,
        })
    }

    fn stop(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        self.armed = false;
        // The private socket follows the successor; the original keeper's child does not.
        let identity = server_pid(&self.socket)
            .and_then(aterm_uds::process::read_identity)
            .unwrap_or(self.last);
        let observed = aterm_uds::process::read_identity(identity.pid);
        if observed == Some(identity) {
            aterm_uds::process::end_proven(
                identity,
                Duration::from_secs(5),
                &mut aterm_uds::process::EndControlLive,
                |_| true,
            )
            .map_err(|e| format!("private GUI cleanup refused: {e:?}"))?;
            wait("private successor native absence", || {
                (aterm_uds::process::process_presence(identity.pid)
                    == aterm_uds::process::ProcessPresence::Absent
                    || aterm_uds::process::read_identity(identity.pid)
                        .is_some_and(|now| now != identity))
                .then_some(())
            })?;
        } else if observed.is_none()
            && aterm_uds::process::process_presence(identity.pid)
                != aterm_uds::process::ProcessPresence::Absent
        {
            return Err("private GUI identity became unreadable during cleanup".into());
        }
        Ok(())
    }
}

impl Drop for OwnedWindow {
    fn drop(&mut self) {
        if let Err(why) = self.stop() {
            eprintln!("installer-handoff cleanup: {why}");
        }
    }
}

fn echo(instance: &Instance, sid: &str, lower: &str, settle: bool) -> Result<(), String> {
    let selector = format!("@{sid}");
    let sent = instance.ctl_stdin(
        &[&selector, "send", "--stdin"],
        format!("{lower}\n").as_bytes(),
    );
    if sent.code != 0 {
        return Err(format!("fixture input failed: {}", sent.first()));
    }
    let pattern = format!("^{}$", lower.to_ascii_uppercase());
    let read = instance.ctl(&[&selector, "await", "match", &pattern, "timeout=60000"]);
    if read.code != 0 {
        return Err(format!(
            "the live filter did not transform input: {}",
            read.first()
        ));
    }
    if settle {
        let idle = instance.ctl(&[&selector, "await", "idle", "1000", "timeout=60000"]);
        if idle.code != 0 {
            return Err(format!("fixture output did not settle: {}", idle.first()));
        }
    }
    Ok(())
}

pub fn cmd(args: &[String]) -> Res {
    crate::stop::install();
    let mut args = args.to_vec();
    let handoffs = handoff_count(&mut args)?;
    let streaming = instance::take_flag(&mut args, "--stream");
    let aterm = instance::resolve_aterm(instance::take_value(&mut args, "--aterm")?)?;
    let out = instance::take_value(&mut args, "--out")?
        .map(PathBuf::from)
        .ok_or_else(|| Fail::usage("installer-handoff needs --out ABS"))?;
    if !args.is_empty() || !out.is_absolute() {
        return Err(Fail::usage(
            "installer-handoff takes --aterm PATH --out ABS [--stream]",
        ));
    }
    execute(&aterm, &out, |aterm, out| {
        run(aterm, out, handoffs, streaming)
    })
}

const STREAM_LIMIT: usize = 10_000;
const STREAM_PAYLOAD: &str = "café λ 界 🙂";

/// Private PTY fixture: numbered output continues until the controller sends `stream_stop`.
pub(crate) fn stream_fixture_main(args: &[String]) -> i32 {
    if !args.is_empty() {
        return 2;
    }
    stream_fixture().map_or(1, |()| 0)
}

fn stream_fixture() -> std::io::Result<()> {
    use std::io::BufRead as _;
    use std::sync::mpsc::{RecvTimeoutError, channel};

    quiet_stream_input()?;
    let (send, receive) = channel();
    std::thread::Builder::new()
        .name("handoff-stream-input".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        })?;
    let mut output = std::io::stdout().lock();
    let mut streaming = false;
    let mut next = 0;
    loop {
        let input = if streaming {
            match receive.recv_timeout(Duration::from_millis(10)) {
                Ok(line) => Some(line),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        } else {
            let Ok(line) = receive.recv() else {
                return Ok(());
            };
            Some(line)
        };
        if let Some(line) = input {
            match line.as_str() {
                "stream_start" => streaming = true,
                "stream_stop" => {
                    streaming = false;
                    writeln!(output, "HANDOFF_END {next}")?;
                }
                _ => {}
            }
            writeln!(output, "{}", line.to_ascii_uppercase())?;
        } else if next == STREAM_LIMIT {
            writeln!(output, "HANDOFF_LIMIT")?;
            streaming = false;
        } else {
            write!(output, "HANDOFF_SEQ {next:06} \x1b[36m")?;
            // Flush individual bytes so multibyte characters can span PTY reads.
            for byte in STREAM_PAYLOAD.as_bytes() {
                output.write_all(std::slice::from_ref(byte))?;
                output.flush()?;
            }
            writeln!(output, "\x1b[0m")?;
            next += 1;
        }
        output.flush()?;
    }
}

fn quiet_stream_input() -> std::io::Result<()> {
    use aterm_uds::{deadline, spawnfd};

    // Keep stty in the fixture's foreground group so terminal job control permits the change.
    let mut child = spawnfd::command("/bin/stty")
        .args(["-echo", "-echonl"])
        .stdin(spawnfd::Stdio::inherit())
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::null())
        .spawn()?;
    let until = Instant::now() + HANG;
    let mut exit = deadline::ExitWait::new(&child);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(std::io::Error::other(format!("stty: {status}"))),
            Err(error) => {
                deadline::settle_child(&mut child);
                return Err(error);
            }
            Ok(None) => {}
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            deadline::kill_within(&mut child, HANG);
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "stty"));
        }
        exit.wait(left);
    }
}

fn search_rows(instance: &Instance, sid: &str, pattern: &str) -> Result<Vec<u64>, String> {
    let selector = format!("@{sid}");
    let found = instance.ctl(&[&selector, "search", pattern, "regex", "case"]);
    parse_search_rows(&found)
}

fn parse_search_rows(found: &instance::Reply) -> Result<Vec<u64>, String> {
    let header = found
        .stderr
        .lines()
        .filter_map(|line| line.strip_prefix("aterm-ctl: "))
        .find(|line| line.starts_with("OK "))
        .unwrap_or_default();
    if found.code != 0 || !header.starts_with("OK ") || header.contains("incomplete") {
        return Err(format!("stream search was incomplete: {}", found.first()));
    }
    let count = header
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse::<usize>().ok())
        .ok_or_else(|| format!("stream search had no count: {header}"))?;
    let mut rows = found
        .stdout
        .lines()
        .map(|line| {
            line.split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .ok_or_else(|| format!("stream search had no row: {line}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() != count {
        return Err("stream search did not return every reported match".into());
    }
    rows.sort_unstable();
    Ok(rows)
}

fn stream_line(instance: &Instance, sid: &str, row: u64) -> Result<String, String> {
    let selector = format!("@{sid}");
    let read = instance.ctl(&[&selector, "line", &row.to_string()]);
    if read.code != 0 {
        return Err(format!("stream row was not retained: {}", read.first()));
    }
    parse_stream_line(&read.stdout)
}

fn parse_stream_line(reply: &str) -> Result<String, String> {
    reply
        .trim_end()
        .strip_prefix("OK ")
        .map(str::to_owned)
        .ok_or_else(|| format!("unexpected stream row reply: {reply}"))
}

fn verify_stream_lines(lines: &[String], count: usize) -> Result<(), String> {
    if count == 0 || count >= STREAM_LIMIT || lines.len() != count {
        return Err(format!(
            "stream expected {count} records, retained {}",
            lines.len()
        ));
    }
    for (index, line) in lines.iter().enumerate() {
        if line != &format!("HANDOFF_SEQ {index:06} {STREAM_PAYLOAD}") {
            return Err(format!(
                "stream record {index} was missing, duplicated or reordered: {line}"
            ));
        }
    }
    Ok(())
}

fn finish_stream(instance: &Instance, sid: &str, out: &Path) -> Result<usize, String> {
    echo(instance, sid, "stream_stop", true)?;
    let ends = search_rows(instance, sid, "^HANDOFF_END [0-9]+$")?;
    let [end] = ends.as_slice() else {
        return Err("the stream did not retain exactly one end record".into());
    };
    let count = stream_line(instance, sid, *end)?
        .strip_prefix("HANDOFF_END ")
        .and_then(|n| n.parse::<usize>().ok())
        .ok_or("the stream end record had no count")?;
    let rows = search_rows(instance, sid, "^HANDOFF_SEQ [0-9]+ ")?;
    let lines = rows
        .into_iter()
        .map(|row| stream_line(instance, sid, row))
        .collect::<Result<Vec<_>, _>>()?;
    verify_stream_lines(&lines, count)?;
    write_new_report(
        &out.join("stream.json"),
        &aterm_json::json!({
            "count": count, "records": lines, "payload": STREAM_PAYLOAD,
            "byte_fragmented": true, "ansi_styled": true,
        }),
    )
    .map_err(|e| e.to_string())?;
    Ok(count)
}

/// Exercise in-place replacement after replacing an exited terminal with a native Settings tab.
pub fn cold_cmd(args: &[String]) -> Res {
    crate::stop::install();
    let mut args = args.to_vec();
    let handoffs = handoff_count(&mut args)?;
    let aterm = instance::resolve_aterm(instance::take_value(&mut args, "--aterm")?)?;
    let out = instance::take_value(&mut args, "--out")?
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| Fail::usage("cold-handoff needs --out ABS"))?;
    if !args.is_empty() {
        return Err(Fail::usage("cold-handoff takes --aterm PATH --out ABS"));
    }
    execute(&aterm, &out, |aterm, out| run_cold(aterm, out, handoffs))
}

fn handoff_count(args: &mut Vec<String>) -> Result<usize, Fail> {
    instance::take_value(args, "--handoffs")?.map_or(Ok(2), |word| {
        word.parse::<usize>()
            .ok()
            .filter(|count| (1..=32).contains(count))
            .ok_or_else(|| Fail::usage("--handoffs must be between 1 and 32"))
    })
}

fn execute(
    aterm: &Path,
    out: &Path,
    probe: impl FnOnce(&Path, &Path) -> Result<(), String>,
) -> Res {
    aterm_uds::private_file::create_private_dir(out).map_err(|e| Fail::die(e.to_string()))?;
    if std::fs::read_dir(out)
        .map_err(|e| Fail::die(e.to_string()))?
        .next()
        .is_some()
    {
        return Err(Fail::usage(
            "handoff probe needs a fresh empty output directory",
        ));
    }
    // Winning this exclusive create owns this run even if two callers both observed an empty folder.
    write_new_report(
        &out.join("run.json"),
        &aterm_json::json!({
            "schema": 1, "result": "started", "pid": std::process::id(),
        }),
    )
    .map_err(|e| Fail::die(e.to_string()))?;
    match probe(aterm, out) {
        Ok(()) => Ok(0),
        Err(error) => {
            let report = aterm_json::json!({"schema": 1, "result": "failed", "error": error});
            if let Err(record_error) = write_new_report(&out.join("failed.json"), &report) {
                eprintln!("handoff failure record: {record_error}");
            }
            Err(Fail::die(error))
        }
    }
}

fn write_new_report(path: &Path, report: &aterm_json::Value) -> std::io::Result<()> {
    let mut file = crate::util::create_new_regular_file(path, 0o600)?;
    writeln!(file, "{report}")
}

struct EvidenceLog {
    source: Option<PathBuf>,
    target: PathBuf,
    attempted: bool,
}

impl EvidenceLog {
    fn new(out: &Path) -> Self {
        Self {
            source: None,
            target: out.join("instance.log"),
            attempted: false,
        }
    }

    fn capture(&mut self) -> Result<(), String> {
        if self.attempted {
            return Ok(());
        }
        self.attempted = true;
        if let Some(source) = &self.source {
            let (mut reader, _) =
                aterm_uds::regular_file::open(source).map_err(|e| e.to_string())?;
            let mut writer = crate::util::create_new_regular_file(&self.target, 0o600)
                .map_err(|e| e.to_string())?;
            std::io::copy(&mut reader, &mut writer).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

impl Drop for EvidenceLog {
    fn drop(&mut self) {
        if let Err(error) = self.capture() {
            eprintln!("handoff instance log: {error}");
        }
    }
}

fn exited_session(text: &str) -> bool {
    text.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next().is_some()
            && words
                .next()
                .is_some_and(|sid| crate::stub::first_sid(sid) == Some(sid))
            && words.next().is_some()
            && words.next() == Some("exited")
    })
}

fn reexec_of(before: aterm_uds::process::Identity, after: aterm_uds::process::Identity) -> bool {
    before.pid == after.pid
        && before.birth == after.birth
        && before
            .idversion
            .zip(after.idversion)
            .is_some_and(|(before, after)| before != after)
}

fn run_cold(aterm: &Path, out: &Path, handoffs: usize) -> Result<(), String> {
    let image = ProbeImage::read(aterm)?;
    image.record(out)?;
    let mut world = World::new(instance::CONFIG_OFF)?;
    world.keep();
    // Declared before the instance: failure captures its log after its cleanup, before World drops.
    let mut evidence = EvidenceLog::new(out);
    let mut env = world.env();
    env.push(("ATERM_DEBUG_SEAMLESS_REEXEC".into(), "1".into()));
    let mut instance = Instance::start(
        &world,
        Launch {
            aterm: image.path.clone(),
            headless: false,
            args: ["--hold", "-e", "/usr/bin/true"]
                .map(str::to_owned)
                .to_vec(),
            env,
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    evidence.source = Some(instance.log.clone());
    instance.await_socket(HANG).map_err(|e| format!("{e:?}"))?;
    let mut window = OwnedWindow::take(&instance.sock)?;
    let original = window.last;
    let mut token_path = instance.sock.as_os_str().to_owned();
    token_path.push(".token");
    let token_path = PathBuf::from(token_path);
    let held = wait("the held terminal's exited state", || {
        let sessions = instance.ctl(&["sessions"]);
        (sessions.code == 0 && exited_session(&sessions.stdout))
            .then(|| crate::stub::first_sid(&sessions.stdout).map(str::to_owned))
            .flatten()
    })?;
    let settings = instance.ctl(&["open", "app", "settings"]);
    if settings.code != 0 {
        return Err(format!("native page did not open: {}", settings.first()));
    }
    let close = instance.ctl(&[&format!("@{held}"), "close"]);
    if close.code != 0 {
        return Err(format!("exited pane did not close: {}", close.first()));
    }
    let mut exec_versions = Vec::new();
    let mut identities = Vec::new();
    for _ in 0..handoffs {
        wait("a native window without terminal sessions", || {
            let sessions = instance.ctl(&["sessions"]);
            (sessions.code == 0 && crate::stub::first_sid(&sessions.stdout).is_none()).then_some(())
        })?;
        let before = std::fs::read(&token_path).map_err(|e| e.to_string())?;
        if before.is_empty() {
            return Err("private control credential is empty".into());
        }
        let applied = instance.ctl(&["update", "apply"]);
        if applied.code != 0 {
            return Err(format!("cold apply failed: {}", applied.first()));
        }
        wait("fresh control credential after in-place exec", || {
            std::fs::read(&token_path)
                .ok()
                .filter(|after| !after.is_empty() && *after != before)
        })?;
        let previous = window.last;
        let successor = wait("the original process to serve after exec", || {
            let pid = server_pid(&instance.sock)?;
            let after = aterm_uds::process::read_identity(pid)?;
            reexec_of(previous, after).then_some(after)
        })?;
        window.last = successor;
        exec_versions.push(successor.idversion);
        identities.push(successor.token());
        let version = instance.ctl(&["version"]);
        if version.code != 0 {
            return Err(format!(
                "cold successor did not answer: {}",
                version.first()
            ));
        }
    }
    window.stop()?;
    instance.stop()?;
    evidence.capture()?;
    image.recheck()?;
    let report = aterm_json::json!({
        "schema": 1, "scenario": "same-image-cold-handoff",
        "pid": original.pid, "replacements": handoffs,
        "same_pid_and_birth": true, "fresh_control_credentials": true,
        "exec_versions": exec_versions,
        "image_path": image.path.to_string_lossy().into_owned(), "image_sha256": image.sha256,
        "initial_identity": original.token(), "successor_identities": identities,
    });
    write_new_report(&out.join("passed.json"), &report).map_err(|e| e.to_string())?;
    world.release();
    println!(
        "PASS: {handoffs} in-place handoffs retained the private window; {}",
        out.display()
    );
    Ok(())
}

fn run(aterm: &Path, out: &Path, handoffs: usize, streaming: bool) -> Result<(), String> {
    let image = ProbeImage::read(aterm)?;
    image.record(out)?;
    let mut world = World::new(instance::CONFIG_OFF)?;
    world.keep();
    let mut evidence = EvidenceLog::new(out);
    let mut env = world.env();
    env.push(("ATERM_DEBUG_SEAMLESS_REEXEC".into(), "1".into()));
    env.push(("ATERM_DEBUG_INSTALLER_CUSTODY".into(), "1".into()));
    let mut args: Vec<String> = ["--columns", "100", "--lines", "30", "-e"]
        .map(str::to_owned)
        .to_vec();
    if streaming {
        args.push(
            std::env::current_exe()
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned(),
        );
        args.push("__handoff-stream".into());
    } else {
        args.extend(["/usr/bin/tr", "a-z", "A-Z"].map(str::to_owned));
    }
    let mut instance = Instance::start(
        &world,
        Launch {
            aterm: image.path.clone(),
            headless: false,
            args,
            env,
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    evidence.source = Some(instance.log.clone());
    instance.await_socket(HANG).map_err(|e| format!("{e:?}"))?;
    let mut window = OwnedWindow::take(&instance.sock)?;
    let parent = window.last.pid;
    let parent_identity = window.last.token();
    let before = wait("the installer fixture's custody record", || {
        record(&instance.log_tail(200), "seeded", parent)
    })?;
    echo(&instance, &before.sid, "installer_before", true)?;
    if streaming {
        echo(&instance, &before.sid, "stream_start", false)?;
    }
    let mut successors = Vec::new();
    let mut identities = Vec::new();
    for index in 0..handoffs {
        let marker = format!("installer_after_{}", index + 1);
        let outgoing = window.last.pid;
        let applied = instance.ctl(&["update", "apply"]);
        if applied.code != 0 {
            return Err(format!("same-image apply failed: {}", applied.first()));
        }
        let successor = wait("a successor on the private socket", || {
            server_pid(&instance.sock).filter(|pid| *pid != outgoing)
        })?;
        window.last = aterm_uds::process::read_identity(successor)
            .ok_or("successor native identity unavailable")?;
        identities.push(window.last.token());
        let after = wait("the successor's recovered installer custody", || {
            record(&instance.log_tail(200), "adopted", successor)
        })?;
        if before != after {
            return Err(format!(
                "installer identity changed: {before:?} -> {after:?}"
            ));
        }
        echo(&instance, &after.sid, &marker, !streaming)?;
        wait("the outgoing GUI to exit", || {
            (!crate::sys::alive(outgoing)).then_some(())
        })?;
        successors.push(successor);
    }
    let stream_records = if streaming {
        Some(finish_stream(&instance, &before.sid, out)?)
    } else {
        None
    };
    window.stop()?;
    instance.stop()?;
    evidence.capture()?;
    image.recheck()?;
    let report = aterm_json::json!({
        "schema": 1, "scenario": "same-image-installer-handoff",
        "parent": parent, "successors": successors,
        "replacements": handoffs,
        "sid": before.sid, "child": before.child, "birth": before.birth,
        "input_transformed_before": true, "input_transformed_after": true,
        "installer_commands_submitted": false, "active_custody_adopted": true,
        "image_path": image.path.to_string_lossy().into_owned(), "image_sha256": image.sha256,
        "initial_identity": parent_identity, "successor_identities": identities,
        "stream_records": stream_records,
    });
    write_new_report(&out.join("passed.json"), &report).map_err(|e| e.to_string())?;
    world.release();
    println!(
        "PASS: {handoffs} live same-image handoffs retained active installer custody; {}",
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_line_reads_the_single_line_cli_reply() {
        assert_eq!(
            parse_stream_line("OK HANDOFF_END 123\n").unwrap(),
            "HANDOFF_END 123"
        );
        assert!(parse_stream_line("ERR evicted\n").is_err());
        assert!(parse_stream_line("HANDOFF_END 123\n").is_err());
    }

    #[test]
    fn stream_search_requires_the_complete_cli_count_and_reads_stdout_rows() {
        let mut reply = instance::Reply {
            code: 0,
            stdout: "92 0 14\n93 0 14\n".into(),
            stderr: "aterm-ctl: OK 2 screen_first=90\n".into(),
        };
        assert_eq!(parse_search_rows(&reply).unwrap(), [92, 93]);
        reply.stderr = "aterm-ctl: OK 2 incomplete moving screen_first=90\n".into();
        assert!(parse_search_rows(&reply).is_err());
        reply.stderr = "aterm-ctl: OK 3 screen_first=90\n".into();
        assert!(parse_search_rows(&reply).is_err());
        reply.stderr.clear();
        assert!(parse_search_rows(&reply).is_err());
    }

    #[test]
    fn streamed_history_must_keep_every_record_once_in_order() {
        let good: Vec<String> = (0..4)
            .map(|i| format!("HANDOFF_SEQ {i:06} café λ 界 🙂"))
            .collect();
        assert!(verify_stream_lines(&good, 4).is_ok());
        let mut missing = good.clone();
        missing.remove(1);
        assert!(verify_stream_lines(&missing, 4).is_err());
        let mut duplicated = good.clone();
        duplicated[2].clone_from(&good[1]);
        assert!(verify_stream_lines(&duplicated, 4).is_err());
        let mut reordered = good.clone();
        reordered.swap(1, 2);
        assert!(verify_stream_lines(&reordered, 4).is_err());
        let mut corrupted = good.clone();
        corrupted[1] = corrupted[1].replace('🙂', "�");
        assert!(verify_stream_lines(&corrupted, 4).is_err());
        assert!(verify_stream_lines(&[], 0).is_err());
        assert!(verify_stream_lines(&good, STREAM_LIMIT).is_err());
    }

    #[test]
    fn handoff_chain_is_explicitly_bounded() {
        assert_eq!(handoff_count(&mut vec![]).unwrap(), 2);
        for count in ["1", "10", "32"] {
            assert_eq!(
                handoff_count(&mut vec!["--handoffs".into(), count.into()]).unwrap(),
                count.parse::<usize>().unwrap()
            );
        }
        for count in ["0", "33", "-1", "unbounded"] {
            assert!(handoff_count(&mut vec!["--handoffs".into(), count.into()]).is_err());
        }
    }

    #[test]
    fn a_changed_image_cannot_produce_a_passing_report() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("image");
        std::fs::write(&path, "first").unwrap();
        let image = ProbeImage::read(&path).unwrap();
        assert!(image.recheck().is_ok());
        std::fs::write(&path, "second").unwrap();
        assert!(image.recheck().is_err());
    }

    #[test]
    fn evidence_created_after_admission_is_never_replaced() {
        fn failed(_: &Path, out: &Path) -> Result<(), String> {
            std::fs::write(out.join("failed.json"), "keep existing record").unwrap();
            Err("a later failure".into())
        }
        let out = aterm_tempfile::tempdir().unwrap();
        assert!(execute(Path::new("unused"), out.path(), failed).is_err());
        assert_eq!(
            std::fs::read_to_string(out.path().join("failed.json")).unwrap(),
            "keep existing record"
        );
        assert!(out.path().join("run.json").is_file());
    }

    #[test]
    fn existing_log_is_not_truncated_by_capture() {
        let out = aterm_tempfile::tempdir().unwrap();
        let source = out.path().join("source");
        std::fs::write(&source, "new log").unwrap();
        std::fs::write(out.path().join("instance.log"), "keep old log").unwrap();
        let mut evidence = EvidenceLog::new(out.path());
        evidence.source = Some(source);
        assert!(evidence.capture().is_err());
        assert_eq!(
            std::fs::read_to_string(out.path().join("instance.log")).unwrap(),
            "keep old log"
        );
    }

    #[test]
    fn a_failed_probe_keeps_its_error_and_log() {
        fn fails(_: &Path, out: &Path) -> Result<(), String> {
            let private = aterm_tempfile::tempdir().unwrap();
            let source = private.path().join("instance.log");
            std::fs::write(&source, "stand-in diagnostics\n").unwrap();
            let mut evidence = EvidenceLog::new(out);
            evidence.source = Some(source);
            Err("stand-in refusal".into())
        }
        let out = aterm_tempfile::tempdir().unwrap();
        assert!(execute(Path::new("unused"), out.path(), fails).is_err());
        let report: aterm_json::Value =
            aterm_json::from_str(&std::fs::read_to_string(out.path().join("failed.json")).unwrap())
                .unwrap();
        assert_eq!(report["error"].as_str(), Some("stand-in refusal"));
        assert_eq!(
            std::fs::read_to_string(out.path().join("instance.log")).unwrap(),
            "stand-in diagnostics\n"
        );
        assert!(!out.path().join("passed.json").exists());
    }

    #[test]
    fn previous_evidence_is_never_overwritten_or_run_again() {
        let out = aterm_tempfile::tempdir().unwrap();
        let previous = out.path().join("previous");
        std::fs::write(&previous, "keep").unwrap();
        assert!(
            execute(Path::new("unused"), out.path(), |_, _| panic!(
                "must not run"
            ))
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(previous).unwrap(), "keep");
        assert!(!out.path().join("failed.json").exists());
    }

    #[test]
    fn in_place_exec_keeps_birth_but_changes_the_exec_version() {
        let identity = |text| aterm_uds::process::Identity::parse(text).unwrap();
        let before = identity("23@45:3");
        assert!(reexec_of(before, identity("23@45:4")));
        assert!(!reexec_of(before, before));
        assert!(!reexec_of(before, identity("24@45:4")));
        assert!(!reexec_of(before, identity("23@46:4")));
    }

    #[test]
    fn cold_precondition_requires_an_exited_session_row() {
        assert!(exited_session(
            "OK 1\n0 s-0123456789abcdefabcd - exited held"
        ));
        assert!(!exited_session(
            "OK 1\n0 s-0123456789abcdefabcd - running exited"
        ));
        assert!(!exited_session("OK 1\n0 invalid - exited held"));
    }

    #[test]
    fn fixture_records_require_a_live_exact_identity_and_the_requested_stage() {
        let line = "aterm-lab-installer stage=adopted gui=17 sid=s-0123456789abcdefabcd child=23 birth=45 active=true";
        assert_eq!(
            record(line, "adopted", 17),
            Some(Custody {
                sid: "s-0123456789abcdefabcd".into(),
                child: 23,
                birth: 45,
            })
        );
        assert!(record(line, "seeded", 17).is_none());
        assert!(record(line, "adopted", 18).is_none());
        for changed in [
            line.replace("birth=45", "birth=0"),
            line.replace("active=true", "active=false"),
            line.replace("child=23", "child=1"),
        ] {
            assert!(record(&changed, "adopted", 17).is_none());
        }
    }
}
