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
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// The whole fleet CLI as a callable: `argv[1..]` in, exits the process on
/// completion (matching its binary-era behavior). Served in-process by the
/// ONE `aterm` binary (`aterm fleet …` / argv0 alias) and by the thin
/// standalone bin.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ! {
    let mode = argv.first().map(|a| a.to_string_lossy().into_owned());
    match mode.as_deref() {
        Some("events") => federate(),
        Some("exec") => dispatch(),
        Some("-h") | Some("--help") | None => usage(0),
        Some(other) => {
            eprintln!("aterm-fleet: unknown mode {other:?}");
            usage(2);
        }
    }
    std::process::exit(0);
}

fn usage(code: i32) -> ! {
    eprint!(
        "aterm-fleet — federate a fleet of aterm sessions into one fabric.\n\n\
         USAGE:\n\
         \x20 aterm-fleet events     merge every live instance's `subscribe events` to stdout\n\
         \x20                        as NDJSON, addressed by astream Subject /fleet/<pid>/events/<sid>\n\
         \x20 aterm-fleet exec       read `@<sid> <verb> [args...]` command lines from stdin,\n\
         \x20                        dispatch each to the fleet, emit an NDJSON result per line\n\n\
         The aterm-ctl binary is $ATERM_CTL, else a sibling of this binary, else PATH.\n"
    );
    std::process::exit(code);
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

/// FEDERATE: one `subscribe … events` per instance, merged to stdout as NDJSON.
///
/// The fleet is NOT frozen at the startup snapshot: a background scanner re-lists
/// the fleet every `RESCAN_SECS` and spawns a fresh subscribe for each instance's
/// newly-appeared sids, so sessions (and whole instances) launched mid-run are
/// federated too. Each sid is subscribed exactly once — the `seen` set gates it so
/// a rescan never double-subscribes a session already streaming.
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

    // Scanner: periodically snapshot the fleet and spawn one subscribe per instance
    // covering ONLY its not-yet-seen sids. Holding a `tx` clone keeps the writer alive
    // across a momentarily-empty fleet, so a later-launched instance still lands.
    let scan_stop = stop.clone();
    let scan_ctl = ctl.clone();
    let scan_tx = tx.clone();
    let scanner = thread::spawn(move || {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        while !scan_stop.load(std::sync::atomic::Ordering::Relaxed) {
            // Group only the not-yet-subscribed sids by owning instance pid; one
            // subscribe covers each instance's fresh sids.
            let mut fresh: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for s in list_sessions(&scan_ctl) {
                if seen.insert(s.sid.clone()) {
                    fresh.entry(s.pid).or_default().push(s.sid);
                }
            }
            for (pid, sids) in fresh {
                eprintln!(
                    "aterm-fleet: federating instance {pid} — {} new sid(s): {}",
                    sids.len(),
                    sids.join(",")
                );
                let ctl = scan_ctl.clone();
                let tx = scan_tx.clone();
                thread::spawn(move || stream_instance(&ctl, &pid, &sids, &tx));
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

/// Stream one instance's `events` and forward each as an NDJSON record. Reads the
/// `sub <local> <sid>` ack map so the compact `<local>` frame tag is resolved back to
/// the stable sid the record is addressed by.
fn stream_instance(ctl: &str, pid: &str, sids: &[String], tx: &mpsc::Sender<String>) {
    let targets = sids
        .iter()
        .map(|s| format!("@{s}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut child = match Command::new(ctl)
        .args(["--pid", pid, "subscribe", &targets, "events"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(err_record(pid, &format!("spawn subscribe failed: {e}")));
            return;
        }
    };
    let Some(out) = child.stdout.take() else {
        return;
    };
    let mut chan_to_sid: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for line in BufReader::new(out).lines().map_while(Result::ok) {
        // Ack map: `sub <local> <sid>` — learn the channel→sid mapping.
        if let Some(rest) = line.strip_prefix("sub ") {
            let mut it = rest.split_whitespace();
            if let (Some(local), Some(sid)) = (it.next(), it.next()) {
                chan_to_sid.insert(local.to_string(), sid.to_string());
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
}
