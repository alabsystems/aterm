// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Best-effort observations of a failed restore before its instance is stopped.

use std::collections::BTreeSet;
use std::time::Instant;

use super::{HANG, Instance, Raw, Wire, common, fields};

// RelayClient also bounds line and byte-framed replies at one MiB.
const REPLY_BYTES: usize = 1024 * 1024;
const TOTAL_BYTES: usize = 8 * 1024 * 1024;
const TARGETS: usize = 128;

enum Reply {
    Line,
    Counted,
    Bytes,
}

#[cfg(unix)]
fn connect(inst: &Instance, until: Instant) -> Result<Wire, String> {
    use std::io::{Error, ErrorKind, Write as _};
    use std::os::fd::AsFd as _;

    let connect = || -> std::io::Result<Wire> {
        let stream = aterm_uds::connect::until(&inst.sock, until)?;
        let token = aterm_ctl::read_token_beside(&inst.sock.to_string_lossy())?;
        if token.contains(['\r', '\n']) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "invalid authentication line",
            ));
        }
        stream.set_nonblocking(true)?;
        let auth = format!("AUTH {token}\n");
        let mut pending = auth.as_bytes();
        while !pending.is_empty() {
            if Instant::now() >= until {
                return Err(Error::new(
                    ErrorKind::TimedOut,
                    "failure capture connection deadline elapsed",
                ));
            }
            match (&stream).write(pending) {
                Ok(0) => {
                    return Err(Error::new(
                        ErrorKind::WriteZero,
                        "authentication write ended",
                    ));
                }
                Ok(written) => pending = &pending[written..],
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    aterm_uds::ready::until(
                        stream.as_fd(),
                        aterm_uds::ready::Interest::Write,
                        until,
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
        stream.set_nonblocking(false)?;
        Ok(aterm_agent::RelayClient::new(stream)
            .bounded(until.saturating_duration_since(Instant::now())))
    };
    connect().map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn connect(_inst: &Instance, _until: Instant) -> Result<Wire, String> {
    Err("deadline-bounded failure capture is unavailable on this platform".into())
}

struct Capture<'a> {
    wire: Wire,
    raw: &'a mut Raw,
    until: Instant,
    left: usize,
}

impl Capture<'_> {
    /// A read-only request. Every exchange shares the remaining capture time and storage.
    /// A poisoned connection records subsequent failures rather than sending another request.
    fn get(&mut self, name: &str, request: &str, kind: Reply) -> Option<String> {
        let remaining = self.until.saturating_duration_since(Instant::now());
        if remaining.is_zero() || self.left == 0 {
            self.raw.note(
                name,
                format!("request={request:?}\nSKIPPED: failure capture budget exhausted"),
            );
            return None;
        }
        self.wire.bound_by(remaining);
        let got = match kind {
            Reply::Line => self.wire.request_line(request),
            Reply::Counted => self
                .wire
                .request_counted_with_limit(request, self.left.min(REPLY_BYTES))
                .map(|(head, body)| format!("{head}\n{body}")),
            Reply::Bytes => self
                .wire
                .request_bytes(request)
                .map(|(head, body)| format!("{head}\n{}", String::from_utf8_lossy(&body))),
        };
        let text = match got {
            Ok(text) => text,
            Err(error) => {
                self.raw
                    .note(name, format!("request={request:?}\nERROR: {error}"));
                return None;
            }
        };
        if text.len() > self.left {
            let mut end = self.left;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            self.raw.note(
                name,
                format!(
                    "request={request:?}\n{}\nTRUNCATED: failure capture byte budget exhausted",
                    &text[..end]
                ),
            );
            self.left = 0;
            return None;
        }
        self.left -= text.len();
        self.raw.note(name, format!("request={request:?}\n{text}"));
        text.starts_with("OK").then_some(text)
    }

    fn session_text(&mut self, sid: &str) {
        let _ = self.get(
            &format!("failure_{sid}_status"),
            &format!("@{sid} status"),
            Reply::Line,
        );
        let _ = self.get(
            &format!("failure_{sid}_text"),
            &format!("@{sid} text"),
            Reply::Counted,
        );
    }
}

/// Capture only after failure. These reads neither move focus nor modify input, documents,
/// metrics, or restore gates; their errors cannot replace the failure that prompted them.
pub(super) fn capture(inst: &Instance, typing: Option<&str>, error: &str, raw: &mut Raw) {
    let until = Instant::now() + HANG;
    raw.note("failure_cause", error);
    let wire = match connect(inst, until) {
        Ok(wire) => wire,
        Err(error) => {
            raw.note("failure_connection", error);
            return;
        }
    };
    // `inspect` and `native-focus` are read-only public commands, but are not in the
    // general RelayClient read-only allowlist. Keep the requests explicit here.
    let mut capture = Capture {
        wire,
        raw,
        until,
        left: TOTAL_BYTES,
    };
    let _ = capture.get("failure_metrics", "metrics", Reply::Line);
    let _ = capture.get("failure_percentiles", "metrics percentiles", Reply::Line);
    let _ = capture.get("failure_native_focus", "native-focus", Reply::Line);
    let _ = capture.get("failure_topology", "inspect app/v1 tabs", Reply::Counted);
    let documents = capture.get("failure_documents", "doc ls", Reply::Counted);

    let mut sessions = BTreeSet::new();
    if let Some(sid) = typing {
        capture.session_text(sid);
        sessions.insert(sid.to_owned());
    }
    if let Some(documents) = documents {
        let mut ids = BTreeSet::new();
        for line in documents.lines().skip(1).filter(|line| !line.is_empty()) {
            match fields(line)
                .get("doc")
                .and_then(|id| id.parse::<u64>().ok())
            {
                Some(id) if id != 0 => {
                    ids.insert(id);
                }
                _ => capture.raw.note("failure_document_id", line),
            }
        }
        if ids.len() > TARGETS {
            capture.raw.note(
                "failure_document_limit",
                format!("{} documents; capturing the first {TARGETS}", ids.len()),
            );
        }
        for id in ids.into_iter().take(TARGETS) {
            let _ = capture.get(
                &format!("failure_document_{id}"),
                &format!("doc read {id}"),
                Reply::Bytes,
            );
        }
    }
    if let Some(roster) = capture.get("failure_sessions", "sessions", Reply::Counted) {
        match common::sessions(&roster) {
            Ok(rows) => {
                if rows.len() > TARGETS {
                    capture.raw.note(
                        "failure_session_limit",
                        format!("{} sessions; capturing the first {TARGETS}", rows.len()),
                    );
                }
                for row in rows.into_iter().take(TARGETS) {
                    if sessions.insert(row.sid.clone()) {
                        capture.session_text(&row.sid);
                    }
                }
            }
            Err(error) => capture.raw.note("failure_session_roster", error),
        }
    }
    for sid in sessions {
        for (suffix, kind) in [
            ("dims", Reply::Line),
            ("resizes", Reply::Counted),
            ("cast drift", Reply::Counted),
        ] {
            let _ = capture.get(
                &format!("failure_{sid}_{}", suffix.replace(' ', "_")),
                &format!("@{sid} {suffix}"),
                kind,
            );
        }
    }
}
