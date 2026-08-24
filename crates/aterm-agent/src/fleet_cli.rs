// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-fleet** — the fleet-federation bridge. It turns N per-instance control
//! sockets into ONE fabric an orchestrator talks to:
//!
//!   aterm-fleet events                 federate: subscribe to `events` on every live
//!                                      instance, merge them, and emit one NDJSON record
//!                                      per event on stdout — each addressed by an astream
//!                                      Subject `/fleet/<pid>/events/<sid>`.
//!
//!   aterm-fleet exec                   dispatch: read command lines from stdin, one per
//!                                      line (`@<sid> <verb> [args…]` — an aterm-ctl verb
//!                                      line), run each against the fleet, and emit an
//!                                      NDJSON result record per command.
//!
//!   aterm-fleet status|manage|...      thin clients of the opt-in operator
//!                                      embedded in the caller's aterm instance.
//!
//! ## Why this shape, and where astream fits
//!
//! astream (the agent-native message bus) is at Phase 0: it ships the pure wire
//! vocabulary (Frame codec, `/`-rooted Subject grammar, partitioner) but no broker
//! yet. So the transport here is NDJSON over stdio — redirect `events` to a file and
//! you have astream's own model, an append-only *replay log*. Every record is
//! addressed by an astream Subject, so when the broker lands the bridge's records
//! ride it UNCHANGED: `events` becomes a publisher to `/fleet/<pid>/events/<sid>` and
//! `exec` a subscriber to `/fleet/commands`. Nothing above the transport changes.
//!
//! The bridge holds NO engine state and evaluates NO predicates — it is pure glue
//! over `aterm-ctl`, so it inherits the control plane's auth, relay, and semantics
//! verbatim (a cross-instance `@<sid>` is relayed by aterm-ctl exactly as always).

use std::io::{BufRead, BufReader, Write};
use std::process::ExitCode;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// The whole fleet CLI as a callable: `argv[1..]` in, process exit code out.
/// Served in-process by the
/// ONE `aterm` binary (`aterm fleet …` / argv0 alias) and by the thin
/// standalone bin.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let mode = argv.first().map(|a| a.to_string_lossy().into_owned());
    match mode.as_deref() {
        Some("events") => {
            federate();
            ExitCode::SUCCESS
        }
        Some("exec") => {
            dispatch();
            ExitCode::SUCCESS
        }
        Some(
            command @ ("status" | "inspect" | "manage" | "unmanage" | "next" | "extend" | "ack"
            | "reconcile" | "clear-fault" | "propose"),
        ) => operator_command(command, &argv[1..]),
        Some("-h") | Some("--help") | None => usage(0),
        Some(other) => {
            eprintln!("aterm-fleet: unknown mode {other:?}");
            usage(2)
        }
    }
}

fn usage(code: u8) -> ExitCode {
    let help = "aterm-fleet — federate a fleet of aterm sessions into one fabric.\n\n\
         USAGE:\n\
         \x20 aterm-fleet events     merge every live instance's `subscribe events` to stdout\n\
         \x20                        as NDJSON, addressed by astream Subject /fleet/<pid>/events/<sid>\n\
         \x20 aterm-fleet exec       read `@<sid> <verb> [args...]` command lines from stdin,\n\
         \x20                        dispatch each to the fleet, emit an NDJSON result per line\n\
         \x20 aterm-fleet status     show the embedded operator and managed allowlist\n\
         \x20 aterm-fleet inspect <event>\n\
         \x20 aterm-fleet manage <sid> | unmanage <sid>\n\
         \x20 aterm-fleet next [timeout=<ms>]\n\
         \x20 aterm-fleet extend <event> <claim-token> [ms=<n>]\n\
         \x20 aterm-fleet ack <event> <claim-token> <no-action|pause|escalate>\n\
         \x20 aterm-fleet reconcile <event> <claim-token> <acted|no-action|pause|escalate> confirm=human\n\
         \x20 aterm-fleet clear-fault confirm=human\n\
         \x20 aterm-fleet propose    read one guarded-turn JSON proposal from stdin\n\n\
         Operator commands use the in-process control client. Legacy events/exec find\n\
         aterm-ctl via $ATERM_CTL, then a sibling of this binary, then PATH.\n\n\
         The embedded operator is EXPERIMENTAL (status reports it) and OFF by default.\n\
         Launch an aterm instance with ATERM_OPERATOR=1 to opt in; it then starts with an\n\
         empty allowlist, so `manage <sid>` is still required before anything is observed.\n\
         Without the opt-in (or with $ATERM_NO_OPERATOR set) its verbs answer\n\
         `ERR operator unavailable`. See docs/OPERATOR-EMBEDDED.md.\n";
    if code == 0 {
        print!("{help}");
    } else {
        eprint!("{help}");
    }
    ExitCode::from(code)
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
            eprintln!("aterm-fleet: {error}");
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

/// Resolve the `aterm-ctl` client: `$ATERM_CTL`, else a sibling of this binary
/// (the co-distributed toolchain layout), else the bare name on `PATH`.
fn ctl_bin() -> String {
    if let Ok(p) = std::env::var("ATERM_CTL") {
        return p;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sib = dir.join("aterm-ctl");
        if sib.exists() {
            return sib.to_string_lossy().into_owned();
        }
    }
    "aterm-ctl".to_string()
}

/// A live session, from `aterm-ctl ls`: `<pid> <local> <sid> <parent> <state> <title>`.
struct Session {
    pid: String,
    sid: String,
}

/// Snapshot the fleet via `aterm-ctl ls`. Empty on any failure (nothing to federate).
fn list_sessions(ctl: &str) -> Vec<Session> {
    let out = match Command::new(ctl).arg("ls").output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let pid = it.next()?.to_string();
            let _local = it.next()?;
            let sid = it.next()?.to_string();
            Some(Session { pid, sid })
        })
        .collect()
}

/// The fleet-rescan cadence: how often FEDERATE re-lists the fleet to pick up
/// instances/sessions that appeared MID-RUN. A whole number of seconds; the
/// scanner sleeps it in 100ms slices so a downstream close is observed promptly.
const RESCAN_SECS: u64 = 1;

/// FEDERATE: ONE `subscribe @* events` per instance, merged to stdout as NDJSON.
///
/// The fleet is NOT frozen at the startup snapshot, and — since the control plane
/// grew a LIVE target set (`@*`) — it no longer needs a new connection to notice a
/// new session either. One subscription per INSTANCE covers every session that
/// instance has now and every one it opens later; the server adopts each and acks
/// it with a `sub <local> <sid>` line, which the reader below already handles
/// wherever it appears in the stream.
///
/// WHAT THIS REPLACES, AND WHY IT WAS BROKEN. The scanner used to key on SIDS: it
/// remembered every sid it had ever seen and spawned a fresh `aterm-ctl subscribe`
/// child for each batch of not-yet-seen ones. Connections, child processes and
/// server push threads therefore scaled with the number of DISCOVERY MOMENTS —
/// i.e. with tab-opens over the whole run — not with the number of instances. The
/// server admits `CONTROL_SUBSCRIPTION_WORKERS` (4) subscriptions; the fifth got
/// `ERR subscription capacity busy`, which is neither an `EVENT` nor a `sub` line,
/// so the child read it, dropped it, and exited. The sid was already in `seen`,
/// permanently — so from the fifth staggered tab-open onward, sessions were simply
/// never federated, silently and forever.
///
/// So the scanner now keys on INSTANCES, and it does not remember failures as if
/// they were successes: a streamer that ends (peer gone, connection dropped, pool
/// momentarily full) releases its pid, and the next rescan re-establishes it. That
/// makes a busy pool a one-second delay instead of permanent blindness.
fn federate() {
    let ctl = ctl_bin();
    eprintln!(
        "aterm-fleet: federating fleet — rescanning every {RESCAN_SECS}s, events -> NDJSON on stdout (Ctrl-C to stop)"
    );

    // Each instance streams on its own thread; a single writer thread (this one, below)
    // serializes the merged NDJSON so records never interleave mid-line. The scanner and
    // every streamer feed the writer over `tx`.
    let (tx, rx) = mpsc::channel::<String>();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Scanner: periodically snapshot the fleet and hold ONE subscribe per instance.
    // Holding a `tx` clone keeps the writer alive across a momentarily-empty fleet,
    // so a later-launched instance still lands.
    let scan_stop = stop.clone();
    let scan_ctl = ctl.clone();
    let scan_tx = tx.clone();
    // Pids with a streamer currently attached. A streamer REMOVES its own pid when
    // it ends, so the next rescan re-establishes it — the retry the sid-keyed
    // design never had.
    let streaming: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let scanner = thread::spawn(move || {
        while !scan_stop.load(std::sync::atomic::Ordering::Relaxed) {
            // Group the live sids by owning instance pid. The sids are needed ONLY
            // as the fallback selector list for a peer too old to know `@*`; on a
            // current peer the list is never used.
            let mut fleet: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for s in list_sessions(&scan_ctl) {
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
                    "aterm-fleet: federating instance {pid} — live target set (@*), {} session(s) now",
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

/// Stream one instance's `events` for the whole run, preferring the LIVE target
/// set and degrading to the frozen list only for a peer that does not know it.
///
/// The compatibility test is behavioural, not a version handshake: a peer that
/// rejects `@*` answers `ERR …`, which `aterm-ctl` reports on STDERR (discarded
/// here) and which is neither an `EVENT` nor a `sub` line — so the child exits
/// having ACKED NOTHING. "Acked at least one channel" is therefore the exact
/// signal, and it is one this bridge can actually observe from the stdout stream
/// it already parses.
fn stream_instance(ctl: &str, pid: &str, sids: &[String], tx: &mpsc::Sender<String>) {
    if stream_targets(ctl, pid, "@*", tx) {
        return;
    }
    // LEGACY PEER: freeze the target list, exactly as this bridge used to. Sessions
    // opened after this point are not covered until the streamer ends and the next
    // rescan re-establishes it with a wider list — lossy, but it is the old
    // behaviour, reached only against an old server.
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
/// NDJSON record. Reads the `sub <local> <sid>` ack map so the compact `<local>`
/// frame tag is resolved back to the stable sid the record is addressed by — and
/// an ADOPTED channel arrives as exactly such a line, mid-stream, which this loop
/// has always handled because the handshake ack was never guaranteed to arrive in
/// one read.
///
/// Returns whether the server acked at least one channel (see [`stream_instance`]).
fn stream_targets(ctl: &str, pid: &str, targets: &str, tx: &mpsc::Sender<String>) -> bool {
    let mut acked = false;
    let mut child = match Command::new(ctl)
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
        return false;
    };
    let mut chan_to_sid: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for line in BufReader::new(out).lines().map_while(Result::ok) {
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
            let sid = chan_to_sid
                .get(local)
                .cloned()
                .unwrap_or_else(|| local.to_string());
            if tx.send(event_record(pid, &sid, body)).is_err() {
                break;
            }
        }
        // (GAP frames and anything else are dropped — events is a lossy digest by design.)
    }
    let _ = child.wait();
    acked
}

/// EXEC: dispatch command lines from stdin (`@<sid> <verb> [args…]`) to the fleet.
fn dispatch() {
    let ctl = ctl_bin();
    eprintln!("aterm-fleet: dispatching commands from stdin (`@<sid> <verb> [args...]` per line)");
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // The command line IS an aterm-ctl verb line (`@<sid> <verb> rest…`); peel the
        // selector + verb for the record's addressing and hand aterm-ctl the REMAINDER
        // as ONE argument, so a `turn`/`send` payload's internal whitespace and quoting
        // survive intact (see `dispatch_argv`) instead of collapsing.
        let (sid, argv) = dispatch_argv(line);
        let rec = match Command::new(&ctl).args(&argv).output() {
            Ok(o) => {
                let stream = if o.status.success() {
                    &o.stdout
                } else {
                    &o.stderr
                };
                result_record(&sid, o.status.success(), &String::from_utf8_lossy(stream))
            }
            Err(e) => result_record(&sid, false, &format!("dispatch failed: {e}")),
        };
        if w.write_all(rec.as_bytes()).is_err() || w.write_all(b"\n").is_err() {
            break;
        }
        let _ = w.flush();
    }
}

/// Parse one dispatch line into `(sid, argv)` for `aterm-ctl`, MIRRORING the server's
/// verb-line split (aterm-gui `control.rs`): peel the leading `@<selector>` on the
/// first space, then the verb on the next — and keep everything AFTER the verb as ONE
/// argument. aterm-ctl rejoins its argv with single spaces, so a lone remainder arg is
/// forwarded byte-for-byte: a `turn`/`send` payload's internal multiple spaces and
/// quotes are preserved, where `split_whitespace` would have collapsed them into
/// separate tokens. `sid` is the selector body (for the result record's astream
/// address), or `-` when the line carries no `@selector` (a self-scoped verb).
fn dispatch_argv(line: &str) -> (String, Vec<String>) {
    // `first`/`tail` and `verb`/`rest` split on the FIRST space only — identical to the
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

fn result_record(sid: &str, ok: bool, reply: &str) -> String {
    format!(
        "{{\"subject\":\"/fleet/commands/{}/result\",\"sid\":\"{}\",\"ok\":{},\"reply\":\"{}\"}}",
        sid,
        sid,
        ok,
        json_escape(reply.trim_end()),
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

/// Minimal JSON string escaping (no serde dep — the crate is std-only). Escapes the
/// mandatory set plus control bytes as `\u00XX`.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ok = result_record("s-a", true, "OK closed s-a\n");
        assert!(ok.contains("\"ok\":true") && ok.contains("OK closed s-a"));
        let bad = result_record("s-a", false, "ERR \"quoted\"\tvalue");
        assert!(bad.contains("\"ok\":false"));
        assert!(bad.contains("\\\"quoted\\\"") && bad.contains("\\t"));
    }

    #[test]
    fn json_escape_handles_control_and_specials() {
        assert_eq!(json_escape("a\"b\\c\nd\u{1}"), "a\\\"b\\\\c\\nd\\u0001");
    }

    #[test]
    fn dispatch_argv_keeps_payload_as_one_verbatim_arg() {
        // Internal multiple spaces and quotes must reach aterm-ctl as ONE argument —
        // `split_whitespace` would have shattered this into five collapsed tokens.
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
