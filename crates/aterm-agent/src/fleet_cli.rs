// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-fleet`: the fleet-federation bridge, turning the per-instance control
//! sockets into one stream an orchestrator on this machine talks to:
//!
//! - `events`: subscribe to `events` on every live instance in the default
//!   socket dir and emit one NDJSON record per event, addressed
//!   `/fleet/<pid>/events/<sid>`. An instance on an explicit `--control-sock`
//!   is listed but not federated (the streamer resolves `--pid` only in the
//!   default dir).
//! - `exec`: run each stdin line (`@<sid> <verb> [args…]`, an aterm-ctl verb
//!   line) against the fleet and emit one NDJSON result per command.
//! - `status|manage|…`: thin clients of the opt-in operator embedded in the
//!   caller's aterm instance.
//!
//! This is local NDJSON glue, not the astream fabric (`crates/aterm-link`
//! carries the cross-machine inbox and posts); the `/fleet/…` subjects are
//! address labels nothing publishes. The bridge holds no engine state: discovery
//! uses the aterm-ctl crate (`fleet_sessions`), and the streamers and `exec`
//! run the aterm-ctl binary ([`ctl_bin`]), inheriting its auth and relay.

use aterm_uds::spawnfd::Stdio;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;

/// The whole fleet CLI as a callable: `argv[1..]` in, process exit code out,
/// served by `aterm fleet …`, the argv0 alias and the standalone bin.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let mode = argv.first().map(|a| a.to_string_lossy().into_owned());
    match mode.as_deref() {
        Some("events") => {
            federate();
            ExitCode::SUCCESS
        }
        Some("exec") => dispatch(),
        Some(
            command @ ("status" | "inspect" | "manage" | "unmanage" | "next" | "extend" | "ack"
            | "reconcile" | "clear-fault" | "propose"),
        ) => operator_command(command, &argv[1..]),
        Some("-h") | Some("--help") | None => usage(),
        Some(_) => {
            eprintln!("aterm fleet: no such mode; run `aterm fleet --help`");
            ExitCode::from(2)
        }
    }
}

fn usage() -> ExitCode {
    let help = "aterm fleet — watch and drive the sessions of every aterm window \
         on this machine.\n\n\
         USAGE:\n\
         \x20 aterm fleet events     merge the `subscribe events` of every live instance in the\n\
         \x20                        default socket dir to stdout as NDJSON, addressed\n\
         \x20                        /fleet/<pid>/events/<sid> (an instance on an explicit\n\
         \x20                        --control-sock is listed by `ls` but not federated)\n\
         \x20 aterm fleet exec       read `@<sid> <verb> [args...]` command lines from stdin,\n\
         \x20                        dispatch each to the fleet, emit an NDJSON result per line\n\
         \x20                        retain stdout/stderr and exit_code; exit 1 if any command fails\n\
         \x20                        stdin failure emits batch_error with input_line and dispatched count\n\
         \x20 aterm fleet status     show the embedded operator and managed allowlist\n\
         \x20 aterm fleet inspect <event>\n\
         \x20 aterm fleet manage <sid> | unmanage <sid>\n\
         \x20 aterm fleet next [timeout=<ms>]\n\
         \x20 aterm fleet extend <event> <claim-token> [ms=<n>]\n\
         \x20 aterm fleet ack <event> <claim-token> <no-action|pause|escalate>\n\
         \x20 aterm fleet reconcile <event> <claim-token> <acted|no-action|pause|escalate> confirm=human\n\
         \x20 aterm fleet clear-fault confirm=human\n\
         \x20 aterm fleet propose    read one guarded-turn JSON proposal from stdin\n\n\
         The operator verbs need `[operator] enabled = true` in aterm.toml (else they\n\
         answer `ERR operator unavailable`) and a `manage <sid>` per session, kept\n\
         across relaunches. See `aterm help fleet`.\n";
    print!("{help}");
    ExitCode::SUCCESS
}

/// Forward a fleet-facing operator command to the normal aterm instance that
/// hosts the caller. The embedded service is already resident; this invocation
/// owns no queue, subscription, or mutation authority and returns after one
/// reply. Call the library directly so `aterm fleet` remains genuinely
/// one-binary and preserves `aterm-ctl`'s exact exit status (including 124).
fn operator_command(command: &str, args: &[std::ffi::OsString]) -> ExitCode {
    let ctl_args = match operator_ctl_args(command, args) {
        Ok(ctl_args) => ctl_args,
        Err(error) => {
            eprintln!("aterm fleet: {error}");
            return ExitCode::from(2);
        }
    };
    aterm_ctl::main_entry(ctl_args)
}

fn operator_ctl_args(
    command: &str,
    args: &[std::ffi::OsString],
) -> Result<Vec<std::ffi::OsString>, &'static str> {
    let mut ctl_args = Vec::with_capacity(args.len() + 2);
    if command == "propose" {
        if !args.is_empty() {
            return Err("propose reads one JSON proposal from stdin and takes no arguments");
        }
        ctl_args.push(std::ffi::OsString::from("operator-propose-bin"));
    } else {
        ctl_args.push(std::ffi::OsString::from("operator"));
        ctl_args.push(std::ffi::OsString::from(command));
        ctl_args.extend_from_slice(args);
    }
    Ok(ctl_args)
}

/// Resolve the `aterm-ctl` binary for the child-process paths that need one
/// (the per-instance `subscribe` streamers and `exec` dispatch): the alias
/// beside this binary, else the bare name on `PATH`. Discovery does not use it
/// (see [`list_sessions`]), so a missing binary fails streaming and dispatch
/// only.
fn ctl_bin() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| crate::drive_cli::sibling_ctl(&exe))
        .map_or_else(
            || "aterm-ctl".to_string(),
            |sib| sib.to_string_lossy().into_owned(),
        )
}

/// A live session from the fleet listing — the `<pid> <local> <sid> …` row of
/// `ls`, reduced to the two columns the bridge addresses by.
struct Session {
    pid: String,
    sid: String,
}

/// Snapshot the fleet in-process, through the `aterm-ctl` crate's own listing.
///
/// `fleet_sessions` classifies failure as data, so a listing that failed (a
/// control-socket directory it could not read, a sandbox refusing AF_UNIX
/// `connect()`, a rewritten token) is distinct from an empty fleet: `Err` is
/// reported through `diag` (rate-limited, see [`ListDiagnostics`]), and only
/// `Ok` can mean an empty fleet.
fn list_sessions(diag: &mut ListDiagnostics) -> Vec<Session> {
    let outcome = aterm_ctl::fleet_sessions();
    if let Some(line) = diag.observe(&outcome) {
        eprintln!("{line}");
    }
    outcome.map(|rows| sessions_from(&rows)).unwrap_or_default()
}

/// The listing rows the bridge can federate: those carrying both columns it
/// addresses by (the instance pid for `--pid`, the sid for the fallback target
/// list). A row too short to name a session is skipped, exactly as the column
/// parse of `ls`'s stdout skipped it.
fn sessions_from(rows: &[aterm_ctl::FleetSession]) -> Vec<Session> {
    rows.iter()
        .filter_map(|row| {
            Some(Session {
                pid: row.pid.to_string(),
                sid: row.sid()?.to_string(),
            })
        })
        .collect()
}

/// How often a still-broken listing repeats its diagnostic: once per this many
/// consecutive failures with the same reason. At [`RESCAN_SECS`] that is one
/// line every five minutes, so an operator who attaches later still learns of
/// it without the stream being drowned.
const LIST_REPEAT_EVERY: u64 = 300;

/// The rate limiter and the phrasing for fleet-listing outcomes.
///
/// The federate loop lists once a second forever, so the diagnostic cannot be
/// unconditional — but silence is the defect being fixed, so it cannot be
/// dropped either. The rule: every distinct reason speaks immediately (the first
/// failure, and every change of cause), an unchanged one speaks again every
/// [`LIST_REPEAT_EVERY`] scans, and a recovery speaks once and rearms.
#[derive(Default)]
struct ListDiagnostics {
    /// The reason last reported and how many consecutive failures have carried
    /// it; `None` while the listing is healthy.
    failing: Option<(String, u64)>,
}

impl ListDiagnostics {
    /// The line to log for this scan's `outcome`, or `None` when the scan says
    /// nothing an operator has not already been told.
    ///
    /// "Nothing to federate" and "discovery is broken" are phrased apart on
    /// purpose: an empty fleet is the bridge working, and an unreachable one is
    /// the bridge blind — the whole point of reporting at all.
    fn observe(
        &mut self,
        outcome: &Result<Vec<aterm_ctl::FleetSession>, aterm_ctl::FleetListError>,
    ) -> Option<String> {
        let error = match outcome {
            Ok(rows) => {
                // Recovery is worth exactly one line, and only after a failure:
                // a healthy loop stays silent.
                let (_, scans) = self.failing.take()?;
                let found = rows.len();
                return Some(format!(
                    "aterm fleet: fleet listing recovered — {found} session(s) after {scans} failed scan(s)"
                ));
            }
            Err(error) => error,
        };
        let scans = match &mut self.failing {
            Some((reason, scans)) if *reason == error.reason => {
                *scans += 1;
                *scans
            }
            slot => {
                *slot = Some((error.reason.clone(), 1));
                1
            }
        };
        if scans > 1 && scans % LIST_REPEAT_EVERY != 0 {
            return None;
        }
        let repeat = if scans > 1 {
            format!(" (still, {scans} scans)")
        } else {
            String::new()
        };
        Some(if error.is_empty_fleet() {
            format!(
                "aterm fleet: nothing to federate{repeat} — {}",
                error.reason
            )
        } else {
            format!(
                "aterm fleet: cannot list the fleet{repeat}: {}",
                error.reason
            )
        })
    }
}

/// The fleet-rescan cadence: how often federation re-lists the fleet to pick
/// up instances that appeared mid-run, in whole seconds. The scanner sleeps it
/// in 100ms slices so a downstream close is observed promptly.
const RESCAN_SECS: u64 = 1;

/// Maximum records waiting behind the one stdout writer. Federation is a live
/// stream, so a blocked downstream must apply backpressure to the per-instance
/// readers instead of retaining an unbounded `String` queue.
const FEDERATED_RECORD_CAPACITY: usize = 32;

/// The events digest is metadata, not a screen dump. Bound the child-facing
/// line before JSON escaping (which can expand control bytes) so one malformed
/// or compromised peer cannot make even the bounded queue retain giant records.
const MAX_FEDERATED_LINE_BYTES: usize = 64 * 1024;

fn federation_channel() -> (mpsc::SyncSender<String>, mpsc::Receiver<String>) {
    mpsc::sync_channel(FEDERATED_RECORD_CAPACITY)
}

/// Read one UTF-8 stream line with an accumulation cap. An oversized or invalid
/// line terminates that instance's streamer; the scanner will retry it on the
/// next pass, while memory stays bounded.
fn read_federated_line<R: BufRead>(reader: &mut R, line: &mut String) -> io::Result<usize> {
    let mut limited = Read::by_ref(reader).take((MAX_FEDERATED_LINE_BYTES + 1) as u64);
    let read = limited.read_line(line)?;
    if read > MAX_FEDERATED_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "federated event line exceeds the 64 KiB bound",
        ));
    }
    Ok(read)
}

/// Terminate a long-lived subscribe child when the local bridge rejects its
/// stream, then always reap it. Killing happens before any best-effort
/// diagnostic publication, so a full output queue cannot retain the child.
fn terminate_stream_child(child: &mut aterm_uds::spawnfd::Child) -> io::Result<()> {
    let _ = child.kill();
    child.wait().map(|_| ())
}

/// Resolve one local subscription channel, retiring its mapping only after the
/// terminal event has captured the stable sid. A long-lived `@*` subscription
/// otherwise remembers every closed tab for the lifetime of the process.
fn resolve_federated_sid(
    channels: &mut std::collections::HashMap<String, String>,
    local: &str,
    body: &str,
) -> String {
    let sid = channels
        .get(local)
        .cloned()
        .unwrap_or_else(|| local.to_string());
    if body == "exited" || body.starts_with("exited ") {
        channels.remove(local);
    }
    sid
}

/// Federate: one `subscribe @* events` per instance, merged to stdout as
/// NDJSON. `@*` covers sessions opened later; the server acks each with a
/// `sub <local> <sid>` line.
///
/// The scanner keys on instances, not sids, because the server admits only
/// `CONTROL_SUBSCRIPTION_WORKERS` (4) subscriptions. A streamer that ends
/// releases its pid and the next rescan re-establishes it.
fn federate() {
    let ctl = ctl_bin();
    eprintln!("aterm fleet: streaming the fleet's events to stdout (Ctrl-C stops)");

    // Each instance streams on its own thread; a single writer thread (this one, below)
    // serializes the merged NDJSON so records never interleave mid-line. The scanner and
    // every streamer feed the writer over `tx`.
    let (tx, rx) = federation_channel();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Scanner: periodically snapshot the fleet and hold one subscribe per
    // instance. Holding a `tx` clone keeps the writer alive across an empty
    // fleet, so a later-launched instance still lands.
    let scan_stop = stop.clone();
    let scan_ctl = ctl.clone();
    let scan_tx = tx.clone();
    // Pids with a streamer currently attached. A streamer removes its own pid when
    // it ends, so the next rescan re-establishes it — the retry the sid-keyed
    // design never had.
    let streaming: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let scanner = thread::spawn(move || {
        // Carried ACROSS rescans: it is what keeps a broken listing from either
        // spamming the stream once a second or going silent after one line.
        let mut listing = ListDiagnostics::default();
        while !scan_stop.load(std::sync::atomic::Ordering::Relaxed) {
            // Group the live sids by owning instance pid. The sids are only the
            // fallback selector list for a peer that does not know `@*`.
            let mut fleet: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for s in list_sessions(&mut listing) {
                fleet.entry(s.pid).or_default().push(s.sid);
            }
            for (pid, sids) in fleet {
                {
                    let mut held = streaming.lock().unwrap_or_else(|p| p.into_inner());
                    if !held.insert(pid.clone()) {
                        continue; // already streaming this instance
                    }
                }
                eprintln!(
                    "aterm fleet: streaming instance {pid}: {} session(s)",
                    sids.len()
                );
                let ctl = scan_ctl.clone();
                let tx = scan_tx.clone();
                let held = streaming.clone();
                let pid_owned = pid.clone();
                thread::spawn(move || {
                    stream_instance(&ctl, &pid_owned, &sids, &tx);
                    // Released, so a dropped/refused subscription is retried on the
                    // next rescan instead of being remembered as a success.
                    held.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&pid_owned);
                });
            }
            // Sleep the cadence in 100ms slices so `stop` is observed within a slice.
            for _ in 0..RESCAN_SECS * 10 {
                if scan_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    });
    drop(tx); // only the scanner + streamers hold senders now

    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    for line in rx {
        if w.write_all(line.as_bytes()).is_err() || w.write_all(b"\n").is_err() {
            break; // downstream (the fabric sink) closed
        }
        let _ = w.flush();
    }
    // Downstream closed: stop the scanner and reap it so the rescan loop is bounded.
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = scanner.join();
}

/// Stream one instance's `events` for the whole run, preferring the live
/// target set and falling back to a frozen list only for a peer that does not
/// know it.
///
/// The test is behavioural: a peer that rejects `@*` answers `ERR …` on the
/// child's stderr (discarded), which is neither an `EVENT` nor a `sub` line, so
/// the child exits having acked nothing. "Acked at least one channel" is the
/// signal, observable from the stdout this bridge parses.
fn stream_instance(ctl: &str, pid: &str, sids: &[String], tx: &mpsc::SyncSender<String>) {
    if stream_targets(ctl, pid, "@*", tx) {
        return;
    }
    // A peer without `@*`: freeze the target list. Sessions opened later are
    // covered once the streamer ends and the next rescan widens the list.
    let targets = sids
        .iter()
        .map(|s| format!("@{s}"))
        .collect::<Vec<_>>()
        .join(",");
    if targets.is_empty() {
        return;
    }
    let _ = stream_targets(ctl, pid, &targets, tx);
}

/// Stream one `subscribe <targets> events` child, forwarding each event as an
/// NDJSON record. Reads the `sub <local> <sid>` ack map to resolve the compact
/// `<local>` frame tag back to the stable sid; an adopted channel arrives as
/// such a line mid-stream.
///
/// Returns whether the server acked at least one channel (see
/// [`stream_instance`]).
fn stream_targets(ctl: &str, pid: &str, targets: &str, tx: &mpsc::SyncSender<String>) -> bool {
    let mut acked = false;
    // On Windows `spawnfd::command` shares the console this verb was typed in
    // with every `aterm-ctl` child here, and a caller with none gets a
    // windowless one.
    let mut child = match aterm_uds::spawnfd::command(ctl)
        .args(["--pid", pid, "subscribe", targets, "events"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(err_record(pid, &format!("spawn subscribe failed: {e}")));
            return false;
        }
    };
    let Some(out) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };
    let mut chan_to_sid: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut reader = BufReader::new(out);
    let mut rejection_diagnostic = None;
    let mut line = String::new();
    loop {
        line.clear();
        match read_federated_line(&mut reader, &mut line) {
            // EOF is only a pipe verdict. The child may have closed stdout
            // while remaining alive, so never enter an unbounded `wait()` on
            // that assumption.
            Ok(0) => {
                break;
            }
            Ok(_) => {
                while line.ends_with(['\r', '\n']) {
                    line.pop();
                }
            }
            Err(error) => {
                rejection_diagnostic = Some(err_record(
                    pid,
                    &format!("subscribe stream rejected: {error}"),
                ));
                break;
            }
        }
        // Ack map: `sub <local> <sid>` — learn the channel→sid mapping.
        if let Some(rest) = line.strip_prefix("sub ") {
            let mut it = rest.split_whitespace();
            if let (Some(local), Some(sid)) = (it.next(), it.next()) {
                chan_to_sid.insert(local.to_string(), sid.to_string());
                acked = true;
            }
            continue;
        }
        // Event frame: `EVENT <local> <kind> <rest…>`.
        if let Some(rest) = line.strip_prefix("EVENT ") {
            let mut it = rest.splitn(2, ' ');
            let local = it.next().unwrap_or("");
            let body = it.next().unwrap_or("");
            let sid = resolve_federated_sid(&mut chan_to_sid, local, body);
            if tx.send(event_record(pid, &sid, body)).is_err() {
                break;
            }
        }
        // (GAP frames and anything else are dropped — events is a lossy digest by design.)
    }
    // A local rejection/disconnect must stop the long-lived subscribe child;
    // waiting while its stdout pipe is still open can otherwise park forever.
    drop(reader);
    let _ = terminate_stream_child(&mut child);
    if let Some(diagnostic) = rejection_diagnostic {
        // Diagnostics are useful but subordinate to resource cleanup. Publish
        // only after the rejected child is gone, and never wait on a full queue.
        let _ = tx.try_send(diagnostic);
    }
    acked
}

/// EXEC: dispatch command lines from stdin (`@<sid> <verb> [args…]`) to the fleet.
fn dispatch() -> ExitCode {
    let ctl = ctl_bin();
    eprintln!("aterm fleet: reading `@<sid> <verb> [args...]` lines from stdin");
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match dispatch_lines(stdin.lock(), &mut stdout.lock(), |argv| {
        aterm_uds::spawnfd::command(&ctl).args(argv).output()
    }) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("aterm fleet: dispatch I/O failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Keep processing after a child failure so every submitted command receives its
/// own result. The aggregate verdict also covers input/output errors, which must
/// never turn a truncated batch into an apparently successful one.
fn dispatch_lines(
    reader: impl BufRead,
    writer: &mut impl Write,
    mut run: impl FnMut(&[String]) -> io::Result<std::process::Output>,
) -> io::Result<bool> {
    let mut all_ok = true;
    // These report input consumption, never whether a target performed an
    // action. `dispatched` includes attempts whose child failed to launch.
    let mut dispatched = 0usize;
    for (index, line) in reader.lines().enumerate() {
        let input_line = index.saturating_add(1);
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                let message = format!("input read failed: {error}");
                let record = batch_error_record(input_line, dispatched, &message);
                if let Err(output_error) = write_dispatch_record(writer, &record) {
                    return Err(dispatch_io_error(
                        &format!("output write failed while reporting {message}"),
                        input_line,
                        dispatched,
                        output_error,
                    ));
                }
                return Err(dispatch_io_error(
                    "input read failed",
                    input_line,
                    dispatched,
                    error,
                ));
            }
        };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // The line is an aterm-ctl verb line; peel the selector and verb for
        // the record's addressing and hand aterm-ctl the remainder as one
        // argument, so payload whitespace and quoting survive (`dispatch_argv`).
        let (sid, argv) = dispatch_argv(line);
        dispatched = dispatched.saturating_add(1);
        let rec = match run(&argv) {
            Ok(o) => {
                all_ok &= o.status.success();
                result_record(
                    &sid,
                    o.status.success(),
                    o.status.code(),
                    &String::from_utf8_lossy(&o.stdout),
                    &String::from_utf8_lossy(&o.stderr),
                )
            }
            Err(e) => {
                all_ok = false;
                result_record(&sid, false, None, "", &format!("dispatch failed: {e}"))
            }
        };
        // The command may already have acted when its result cannot be
        // written. Do not attempt another stdout record or replay the command;
        // return enough context for the caller's stderr diagnostic instead.
        write_dispatch_record(writer, &rec).map_err(|error| {
            dispatch_io_error("output write failed", input_line, dispatched, error)
        })?;
    }
    Ok(all_ok)
}

fn write_dispatch_record(writer: &mut impl Write, record: &str) -> io::Result<()> {
    writer.write_all(record.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn dispatch_io_error(
    stage: &str,
    input_line: usize,
    dispatched: usize,
    error: io::Error,
) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{stage} (input_line={input_line} dispatched={dispatched}): {error}"),
    )
}

/// Parse one dispatch line into `(sid, argv)` for `aterm-ctl`, mirroring the
/// server's verb-line split (aterm-gui `control.rs`): peel `@<selector>`, then
/// the verb, and keep the rest as one argument, which aterm-ctl forwards
/// byte-for-byte. `sid` is the selector body, or `-` when the line carries no
/// `@selector`.
fn dispatch_argv(line: &str) -> (String, Vec<String>) {
    // `first`/`tail` and `verb`/`rest` split on the first space only — identical to the
    // server's `split_once(' ')`, which is what makes the remainder verbatim.
    let (selector, after_sel) = match line.split_once(' ') {
        Some((first, tail)) if first.starts_with('@') => (Some(first), tail),
        // A bare `@selector` with no verb: keep the selector, empty remainder (aterm-ctl
        // / the server answer `ERR unknown verb`, matching a direct invocation).
        _ if line.starts_with('@') => (Some(line), ""),
        _ => (None, line),
    };
    let (verb, rest) = match after_sel.split_once(' ') {
        Some((v, r)) => (v, r),
        None => (after_sel, ""),
    };
    let sid = selector
        .and_then(|s| s.strip_prefix('@'))
        .unwrap_or("-")
        .to_string();
    let mut argv: Vec<String> = Vec::with_capacity(3);
    if let Some(sel) = selector {
        argv.push(sel.to_string());
    }
    if !verb.is_empty() {
        argv.push(verb.to_string());
    }
    if !rest.is_empty() {
        argv.push(rest.to_string());
    }
    (sid, argv)
}

// ── NDJSON records, addressed by astream Subject (`/`-rooted, wildcard-free) ──

fn event_record(pid: &str, sid: &str, body: &str) -> String {
    format!(
        "{{\"subject\":\"/fleet/{}/events/{}\",\"instance\":\"{}\",\"sid\":\"{}\",\"event\":\"{}\"}}",
        pid,
        sid,
        pid,
        sid,
        json_escape(body),
    )
}

fn result_record(
    sid: &str,
    ok: bool,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> String {
    // `reply` is the trimmed stream (stdout on success, stderr on failure);
    // both streams are also kept with trailing newlines.
    let reply = if ok { stdout } else { stderr };
    let exit_code = exit_code.map_or_else(|| "null".to_string(), |code| code.to_string());
    format!(
        "{{\"subject\":\"/fleet/commands/{}/result\",\"sid\":\"{}\",\"ok\":{},\"reply\":\"{}\",\"exit_code\":{},\"stdout\":\"{}\",\"stderr\":\"{}\"}}",
        json_escape(sid),
        json_escape(sid),
        ok,
        json_escape(reply.trim_end()),
        exit_code,
        json_escape(stdout),
        json_escape(stderr),
    )
}

fn batch_error_record(input_line: usize, dispatched: usize, error: &str) -> String {
    format!(
        "{{\"subject\":\"/fleet/commands/batch_error\",\"kind\":\"batch_error\",\"ok\":false,\"input_line\":{input_line},\"dispatched\":{dispatched},\"error\":\"{}\"}}",
        json_escape(error),
    )
}

fn err_record(pid: &str, msg: &str) -> String {
    format!(
        "{{\"subject\":\"/fleet/{}/events\",\"instance\":\"{}\",\"error\":\"{}\"}}",
        pid,
        pid,
        json_escape(msg),
    )
}

/// A JSON string body: the control path's one escape
/// ([`aterm_codec::json::escape`]). A record carries a command's stdout — an
/// `inbox get` body, a screen's text — and NDJSON on a terminal is read raw,
/// so every character a terminal acts on is `\uXXXX` as well as RFC 8259's
/// set (round fifteen, OUT-2).
fn json_escape(s: &str) -> String {
    aterm_codec::json::escape(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use aterm_uds::spawnfd::Command;

    #[test]
    fn federation_queue_is_bounded_and_backpressures_at_capacity() {
        let (tx, rx) = federation_channel();
        for index in 0..FEDERATED_RECORD_CAPACITY {
            tx.try_send(index.to_string()).expect("capacity slot");
        }
        assert!(
            matches!(
                tx.try_send("overflow".to_string()),
                Err(mpsc::TrySendError::Full(_))
            ),
            "a blocked stdout writer must bound retained event records"
        );
        drop(rx);
    }

    #[test]
    fn federated_line_reader_caps_accumulation() {
        let mut at_cap = vec![b'x'; MAX_FEDERATED_LINE_BYTES - 1];
        at_cap.push(b'\n');
        let mut line = String::new();
        assert_eq!(
            read_federated_line(&mut io::Cursor::new(at_cap), &mut line).unwrap(),
            MAX_FEDERATED_LINE_BYTES
        );

        let over = vec![b'x'; MAX_FEDERATED_LINE_BYTES + 1];
        line.clear();
        let error = read_federated_line(&mut io::Cursor::new(over), &mut line).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(line.len() <= MAX_FEDERATED_LINE_BYTES + 1);
    }

    #[test]
    fn exited_channels_do_not_accumulate_for_the_stream_lifetime() {
        let mut channels = std::collections::HashMap::new();
        for index in 0..1_000 {
            let local = index.to_string();
            let sid = format!("sid-{index}");
            channels.insert(local.clone(), sid.clone());
            assert_eq!(
                resolve_federated_sid(&mut channels, &local, "output bytes=1"),
                sid
            );
            assert_eq!(channels.len(), 1, "the live mapping must remain");
            assert_eq!(resolve_federated_sid(&mut channels, &local, "exited"), sid);
            assert!(channels.is_empty(), "the terminal mapping must be retired");
        }
    }

    /// The cleanup seam must not wait for a long-lived subscribe child after a
    /// local rejection. This directly guards the helper used after oversized,
    /// invalid, and disconnected stream exits.
    #[cfg(unix)]
    #[test]
    fn rejected_stream_child_is_killed_and_reaped_promptly() {
        use std::os::unix::process::ExitStatusExt as _;

        // "Terminated, not waited for" is read off the child's own status: a
        // subscriber cleanup killed ends by SIGKILL, one it waited out exits 0
        // when its sleep does. So the property needs no stopwatch. The bound is
        // a minute's hang detector (AGENTS.md), as in the test below: the child
        // lives five, so only a wait for the child's own exit reaches it. It
        // does not time the kill: a cleanup that killed a bounded while late
        // (under a minute) would pass. (It was a 3 s child under a 1 s bound, a
        // kill and a reap timed against the scheduler; then a 60 s child under
        // half its life.)
        const CHILD_LIFETIME_S: u64 = 300;
        let mut child = Command::new("sh")
            // Replace the shell so terminating `child` cannot orphan a sleeping
            // grandchild in the test process tree.
            .args(["-c", &format!("exec sleep {CHILD_LIFETIME_S}")])
            .spawn()
            .expect("spawn long-lived fake subscriber");
        let started = std::time::Instant::now();
        terminate_stream_child(&mut child).expect("kill and reap subscriber");
        let elapsed = started.elapsed();
        let status = child
            .try_wait()
            .expect("query reaped child")
            .expect("the terminated subscriber must already be reaped");
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "cleanup waited for the long-lived child instead of terminating it: {status:?}"
        );
        assert!(
            elapsed < aterm_spec::wait::MUST_HAPPEN,
            "cleanup returned after {elapsed:?}, the child's own {CHILD_LIFETIME_S} s \
             showing through"
        );
    }

    /// A subscriber may close stdout without exiting. The stream bridge must
    /// treat that disconnect exactly like a local rejection, not block forever
    /// waiting for a process that no longer has a readable pipe.
    #[cfg(unix)]
    #[test]
    fn subscriber_that_closes_stdout_is_terminated_promptly() {
        let script = std::env::temp_dir().join(format!(
            "aterm-fleet-closed-stdout-{}-{}.sh",
            std::process::id(),
            aterm_time::since_epoch()
                .expect("clock after epoch")
                .as_nanos()
        ));
        // THE BOUND IS A HANG DETECTOR, not a latency budget: EOF on a LIVE
        // child's stdout must end the stream instead of waiting the child out.
        // The child sleeps five minutes, so a regression returns at five
        // minutes or later and any return inside a minute (AGENTS.md's hang
        // detector) came from the EOF; tighter bounds failed under load. The
        // five minutes are paid only when the test FAILS.
        const CHILD_LIFETIME_S: u64 = 300;
        aterm_tempfile::stand_in(
            &script,
            &format!("#!/bin/sh\nexec 1>&-\nexec sleep {CHILD_LIFETIME_S}\n"),
        )
        .expect("lay fake subscriber");
        let (tx, _rx) = federation_channel();

        let started = std::time::Instant::now();
        assert!(!stream_targets(
            script.to_str().expect("utf8 temp path"),
            "1",
            "@*",
            &tx,
        ));
        let elapsed = started.elapsed();
        assert!(
            elapsed < aterm_spec::wait::MUST_HAPPEN,
            "EOF from a live child escaped prompt cleanup: returned after {elapsed:?}, \
             which is the child's own {CHILD_LIFETIME_S} s sleep showing through rather \
             than EOF"
        );
        let _ = std::fs::remove_file(script);
    }

    #[test]
    fn event_record_is_astream_addressed_and_valid_json_shape() {
        let r = event_record(
            "67939",
            "s-ab12",
            "turn 3 submitted=1 status=settled dur_ms=540",
        );
        assert!(r.contains("\"subject\":\"/fleet/67939/events/s-ab12\""));
        assert!(r.contains("\"sid\":\"s-ab12\""));
        assert!(r.contains("status=settled"));
        assert!(r.starts_with('{') && r.ends_with('}'));
    }

    #[test]
    fn result_record_reports_ok_and_escapes() {
        let ok = result_record("s-a", true, Some(0), "OK closed s-a\n", "");
        assert!(ok.contains("\"ok\":true") && ok.contains("OK closed s-a"));
        let bad = result_record("s-a", false, Some(1), "", "ERR \"quoted\"\tvalue");
        assert!(bad.contains("\"ok\":false"));
        assert!(bad.contains("\\\"quoted\\\"") && bad.contains("\\t"));
    }

    #[test]
    fn result_record_preserves_turn_screen_verdict_and_exit_status() {
        let record = result_record(
            "s-a",
            true,
            Some(0),
            "answer\n\n",
            "aterm-ctl: turn submitted=1 status=settled\n",
        );
        assert_eq!(
            record,
            concat!(
                "{\"subject\":\"/fleet/commands/s-a/result\",\"sid\":\"s-a\",",
                "\"ok\":true,\"reply\":\"answer\",\"exit_code\":0,",
                "\"stdout\":\"answer\\n\\n\",",
                "\"stderr\":\"aterm-ctl: turn submitted=1 status=settled\\n\"}"
            )
        );
        let timeout = result_record("s-a", false, Some(124), "partial\n", "timeout\n");
        assert!(timeout.contains("\"exit_code\":124"));
        assert!(timeout.contains("\"stdout\":\"partial\\n\""));
        assert!(timeout.contains("\"reply\":\"timeout\""));
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_reports_child_failure_and_still_processes_the_remaining_batch() {
        let mut output = Vec::new();
        let mut called = Vec::new();
        let all_ok = dispatch_lines(
            io::Cursor::new("# ignored\n\n@s-a turn first\n@s-b text\n"),
            &mut output,
            |argv| {
                called.push(argv.to_vec());
                let script = if called.len() == 1 {
                    "printf 'partial screen\\n'; printf 'turn status=timeout\\n' >&2; exit 124"
                } else {
                    "printf 'ready\\n'; printf 'second verdict\\n' >&2"
                };
                Command::new("/bin/sh").args(["-c", script]).output()
            },
        )
        .unwrap();
        assert!(!all_ok, "a failed child makes the whole batch fail");
        assert_eq!(
            called.len(),
            2,
            "a failed child must not discard later commands"
        );
        assert_eq!(called[0], ["@s-a", "turn", "first"]);
        assert_eq!(called[1], ["@s-b", "text"]);
        let records = String::from_utf8(output).unwrap();
        let records: Vec<_> = records.lines().collect();
        assert_eq!(records.len(), 2);
        assert!(records[0].contains("\"ok\":false"));
        assert!(records[0].contains("\"exit_code\":124"));
        assert!(records[0].contains("\"stdout\":\"partial screen\\n\""));
        assert!(records[1].contains("\"ok\":true"));
        assert!(records[1].contains("\"exit_code\":0"));
        assert!(records[1].contains("\"stderr\":\"second verdict\\n\""));
    }

    #[test]
    fn dispatch_launch_failure_has_no_fabricated_exit_code() {
        let mut output = Vec::new();
        let all_ok = dispatch_lines(io::Cursor::new("@s-a text\n"), &mut output, |_| {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "missing control client",
            ))
        })
        .unwrap();
        assert!(!all_ok);
        let record = String::from_utf8(output).unwrap();
        assert!(record.contains("\"ok\":false"));
        assert!(record.contains("\"exit_code\":null"));
        assert!(record.contains("\"stderr\":\"dispatch failed: missing control client\""));
    }

    #[test]
    fn dispatch_input_failure_is_not_silently_a_successful_empty_batch() {
        let mut output = Vec::new();
        let result = dispatch_lines(io::Cursor::new([0xff, b'\n']), &mut output, |_| {
            panic!("invalid UTF-8 input must not dispatch a command")
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        let record = String::from_utf8(output).unwrap();
        assert_eq!(record.lines().count(), 1, "one terminal batch error");
        assert!(record.contains("\"kind\":\"batch_error\""));
        assert!(record.contains("\"ok\":false"));
        assert!(record.contains("\"input_line\":1,\"dispatched\":0"));
        assert!(record.contains("\"error\":\"input read failed:"));
        let mut output = Vec::new();
        assert!(
            dispatch_lines(io::Cursor::new("# only a comment\n"), &mut output, |_| {
                panic!("comments are not commands")
            })
            .unwrap(),
            "an empty valid batch remains successful"
        );
        assert!(output.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_invalid_input_reports_the_executed_prefix_and_stops() {
        let mut output = Vec::new();
        let mut calls = 0;
        let error = dispatch_lines(
            io::Cursor::new(b"# comment\n\n@s-a text\n\xff\n@s-b text\n"),
            &mut output,
            |_| {
                calls += 1;
                Command::new("/bin/sh")
                    .args(["-c", "printf 'first command finished\\n'"])
                    .output()
            },
        )
        .unwrap_err();
        assert_eq!(calls, 1, "never execute input after the invalid line");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("input_line=4 dispatched=1"));
        let records = String::from_utf8(output).unwrap();
        let records: Vec<_> = records.lines().collect();
        assert_eq!(
            records.len(),
            2,
            "the result is followed by the batch error"
        );
        assert!(records[0].contains("\"ok\":true"));
        assert!(records[0].contains("first command finished"));
        assert!(records[1].contains("\"subject\":\"/fleet/commands/batch_error\""));
        assert!(records[1].contains("\"kind\":\"batch_error\""));
        assert!(records[1].contains("\"input_line\":4,\"dispatched\":1"));
    }

    #[test]
    fn dispatch_broken_output_reports_possible_effects_without_retrying() {
        struct BrokenWriter {
            writes: usize,
        }
        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.writes += 1;
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed receiver"))
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("a failed write cannot reach flush")
            }
        }
        let mut writer = BrokenWriter { writes: 0 };
        let mut calls = 0;
        let error = dispatch_lines(
            io::Cursor::new("# comment\n@s-a text\n@s-b text\n"),
            &mut writer,
            |_| {
                calls += 1;
                Err(io::Error::new(io::ErrorKind::NotFound, "missing client"))
            },
        )
        .unwrap_err();
        assert_eq!(calls, 1, "a failed result write stops further dispatch");
        assert_eq!(writer.writes, 1, "never retry on a broken output stream");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(error.to_string().contains("output write failed"));
        assert!(error.to_string().contains("input_line=2 dispatched=1"));
    }

    #[test]
    fn json_escape_handles_control_and_specials() {
        assert_eq!(json_escape("a\"b\\c\nd\u{1}"), "a\\\"b\\\\c\\nd\\u0001");
    }

    #[test]
    fn dispatch_argv_keeps_payload_as_one_verbatim_arg() {
        // Internal multiple spaces and quotes must reach aterm-ctl as one
        // argument.
        let (sid, argv) = dispatch_argv("@s-ab12 turn please run  the  build \"a b\"");
        assert_eq!(sid, "s-ab12");
        assert_eq!(
            argv,
            vec![
                "@s-ab12".to_string(),
                "turn".to_string(),
                "please run  the  build \"a b\"".to_string(),
            ]
        );
    }

    #[test]
    fn dispatch_argv_verb_only_and_bare_selector() {
        // A verb with no payload: selector + verb, no trailing empty arg.
        let (sid, argv) = dispatch_argv("@s-a screen");
        assert_eq!(sid, "s-a");
        assert_eq!(argv, vec!["@s-a".to_string(), "screen".to_string()]);
        // A bare `@selector` with no verb: just the selector (server → ERR unknown verb).
        let (sid, argv) = dispatch_argv("@s-a");
        assert_eq!(sid, "s-a");
        assert_eq!(argv, vec!["@s-a".to_string()]);
    }

    #[test]
    fn dispatch_argv_without_selector_runs_verb_self_scoped() {
        let (sid, argv) = dispatch_argv("text");
        assert_eq!(sid, "-");
        assert_eq!(argv, vec!["text".to_string()]);
    }

    /// A listing row as `aterm-ctl` hands one over.
    fn row(pid: u32, row: &str) -> aterm_ctl::FleetSession {
        aterm_ctl::FleetSession {
            pid,
            row: row.to_string(),
            is_self: false,
        }
    }

    /// A classified listing failure, as `fleet_sessions` reports one.
    fn list_error(code: u8, reason: &str) -> aterm_ctl::FleetListError {
        aterm_ctl::FleetListError {
            reason: reason.to_string(),
            code,
        }
    }

    /// EQUIVALENCE: a live fleet federates exactly as before. The columns the
    /// scanner keys on — the hosting pid and the sid — are the ones the `ls` row
    /// always carried, and a row too short to name a session is still skipped
    /// rather than federated as a phantom.
    #[test]
    fn a_live_listing_maps_to_the_same_pid_and_sid_columns_as_the_ls_parse() {
        let listed = sessions_from(&[
            row(
                67939,
                "0 s-ab12 - alive sh meta=0 window=1 active=1 wfocus=1 detail=claude",
            ),
            row(
                67939,
                "1 s-cd34 s-ab12 alive sh meta=1 window=1 active=0 wfocus=1 detail=-",
            ),
            row(70001, "0 s-ef56 - alive sh meta=0"),
            // An explicit-socket instance whose graph entry named no pid: the `0`
            // placeholder rides through exactly as it did through the printed row.
            row(0, "0 s-0000 - alive sh meta=0"),
            // Too short to name a session — never federated.
            row(67939, "9"),
        ]);
        let pairs: Vec<(&str, &str)> = listed
            .iter()
            .map(|s| (s.pid.as_str(), s.sid.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("67939", "s-ab12"),
                ("67939", "s-cd34"),
                ("70001", "s-ef56"),
                ("0", "s-0000"),
            ]
        );
    }

    /// A listing that failed reaches the operator as a diagnostic: the
    /// snapshot is empty either way, so the logged line is what tells "nothing
    /// to federate" from a bridge that has gone blind.
    #[test]
    fn a_failed_listing_is_reported_and_never_read_as_an_empty_fleet() {
        let mut diag = ListDiagnostics::default();
        let broken = Err(list_error(
            2,
            "found 1 control socket in /run/aterm but could not reach any:\n  \
             aterm-67939.sock  connect: Operation not permitted — a sandbox is refusing \
             AF_UNIX connect()",
        ));
        let line = diag
            .observe(&broken)
            .expect("a failure must never pass silently");
        assert!(line.contains("cannot list the fleet"), "{line}");
        // The cause from the listing's own error.
        assert!(
            line.contains("a sandbox is refusing AF_UNIX connect()"),
            "{line}"
        );

        // An EMPTY fleet is a different sentence: the bridge is working, there is
        // simply nothing to federate. An operator must be able to tell them apart
        // from the stream alone.
        let mut diag = ListDiagnostics::default();
        let empty = Err(list_error(
            1,
            "no live aterm instances found (looked in /run/aterm)",
        ));
        let line = diag.observe(&empty).expect("said once");
        assert!(line.contains("nothing to federate"), "{line}");
        assert!(!line.contains("cannot list"), "{line}");
    }

    /// The 1 Hz rescan must not become a 1 Hz log. A distinct reason speaks at
    /// once; the same one repeats only every [`LIST_REPEAT_EVERY`] scans, so an
    /// operator who attached late still learns discovery is broken.
    #[test]
    fn a_repeated_failure_speaks_once_per_distinct_reason_then_periodically() {
        let mut diag = ListDiagnostics::default();
        let denied = Err(list_error(2, "connect: Operation not permitted"));
        assert!(diag.observe(&denied).is_some(), "the first failure speaks");
        for scan in 2..LIST_REPEAT_EVERY {
            assert!(
                diag.observe(&denied).is_none(),
                "scan {scan} of an unchanged failure must stay quiet"
            );
        }
        let repeat = diag
            .observe(&denied)
            .expect("the periodic reminder is what a late operator sees");
        assert!(repeat.contains("still, 300 scans"), "{repeat}");

        // A CHANGED cause is news, whatever the count: the fleet moved from
        // "denied" to "the directory vanished" and the operator must hear it.
        let missing = Err(list_error(
            2,
            "control-socket directory /run/aterm does not exist",
        ));
        let changed = diag.observe(&missing).expect("a new cause speaks at once");
        assert!(changed.contains("does not exist"), "{changed}");
        assert!(!changed.contains("still,"), "a new cause is not a repeat");
    }

    /// Recovery closes the story exactly once, and a healthy loop says nothing
    /// at all — the property that lets an operator treat any listing line on the
    /// stream as significant.
    #[test]
    fn a_recovered_listing_says_so_once_and_a_healthy_loop_stays_silent() {
        let mut diag = ListDiagnostics::default();
        let broken = Err(list_error(2, "connect: Operation not permitted"));
        assert!(diag.observe(&broken).is_some());
        assert!(diag.observe(&broken).is_none());

        let live = Ok(vec![row(67939, "0 s-ab12 - alive sh meta=0")]);
        let back = diag.observe(&live).expect("recovery is reported once");
        assert!(back.contains("recovered"), "{back}");
        assert!(
            back.contains("1 session(s) after 2 failed scan(s)"),
            "{back}"
        );
        assert!(
            diag.observe(&live).is_none(),
            "a healthy listing is silent, scan after scan"
        );

        // …and the next failure is news again, not a suppressed repeat.
        assert!(diag.observe(&broken).is_some());
    }

    #[test]
    fn operator_commands_build_exact_text_or_binary_control_frames() {
        let clear =
            operator_ctl_args("clear-fault", &[std::ffi::OsString::from("confirm=human")]).unwrap();
        assert_eq!(
            clear,
            ["operator", "clear-fault", "confirm=human"].map(std::ffi::OsString::from)
        );

        let proposal = operator_ctl_args("propose", &[]).unwrap();
        assert_eq!(proposal, [std::ffi::OsString::from("operator-propose-bin")]);
        assert!(operator_ctl_args("propose", &[std::ffi::OsString::from("inline")]).is_err());
    }
}
