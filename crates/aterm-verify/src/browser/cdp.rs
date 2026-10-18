// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Chrome DevTools protocol over `--remote-debugging-pipe`: the browser
//! reads commands on its fd 3 and writes answers and events on its fd 4,
//! each message one JSON text ended by a NUL. Flat sessions: a command for a
//! page carries its `sessionId`, as do that page's events.
//!
//! Every wait is a one-minute hang detector, never a latency budget. When
//! this process goes, its end of fd 3 closes and the browser shuts itself
//! down (Chromium closes on a pipe EOF), so an interrupted run leaves no
//! browser behind.

use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use aterm_json::Value;

/// How long any one answer or event may take before the lane calls it hung.
pub(crate) const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// One DevTools connection.
pub(crate) struct Cdp {
    to: std::io::PipeWriter,
    from: Receiver<Value>,
    next: u64,
    /// Events read while waiting for something else, oldest first.
    events: Vec<Value>,
}

/// The NUL-ended messages in `pending`, parsed and drained; a partial message
/// stays. A message that does not parse is reported as such rather than lost.
pub(crate) fn split_messages(pending: &mut Vec<u8>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(end) = pending.iter().position(|&b| b == 0) {
        let text: Vec<u8> = pending.drain(..=end).take(end).collect();
        out.push(
            aterm_json::from_slice(&text)
                .unwrap_or_else(|e| aterm_json::json!({"unparsed": e.to_string()})),
        );
    }
    out
}

/// Forward every message `from` carries to `tx` until EOF.
fn pump(mut from: std::io::PipeReader, tx: &Sender<Value>) {
    let (mut pending, mut buf) = (Vec::new(), vec![0u8; 1 << 16]);
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => pending.extend_from_slice(&buf[..n]),
        }
        for m in split_messages(&mut pending) {
            if tx.send(m).is_err() {
                return;
            }
        }
    }
}

impl Cdp {
    /// A connection over the two pipe ends this process keeps: `to` is the
    /// browser's fd 3, `from` its fd 4.
    pub(crate) fn new(to: std::io::PipeWriter, from: std::io::PipeReader) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || pump(from, &tx));
        Self {
            to,
            from: rx,
            next: 0,
            events: Vec::new(),
        }
    }

    /// The next message, within what is left of the hang detector started at
    /// `t0`; `what` names the wait in the error.
    fn recv(&mut self, t0: Instant, what: &str) -> Result<Value, String> {
        let left = HANG_DETECTOR.saturating_sub(t0.elapsed());
        match self.from.recv_timeout(left) {
            Ok(m) => Ok(m),
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "no {what} within {} s (a hang detector, not a budget)",
                HANG_DETECTOR.as_secs()
            )),
            Err(RecvTimeoutError::Disconnected) => Err(format!(
                "the browser closed its DevTools pipe before {what}"
            )),
        }
    }

    /// Send `method` with `params` (to the page `session`, else the browser)
    /// and wait for its answer: the `result`, or the protocol's error.
    pub(crate) fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        let mut msg = aterm_json::json!({"id": id, "method": method, "params": params});
        if let (Some(s), Some(map)) = (session, msg.as_object_mut()) {
            map.insert("sessionId".to_string(), Value::String(s.to_string()));
        }
        let mut bytes =
            aterm_json::to_vec(&msg).map_err(|e| format!("could not encode {method}: {e}"))?;
        bytes.push(0);
        self.to
            .write_all(&bytes)
            .and_then(|()| self.to.flush())
            .map_err(|e| format!("could not send {method}: {e}"))?;
        let t0 = Instant::now();
        loop {
            let m = self.recv(t0, &format!("answer to {method}"))?;
            if m.get("id").and_then(Value::as_u64) != Some(id) {
                self.events.push(m);
                continue;
            }
            if let Some(err) = m.get("error") {
                return Err(format!(
                    "{method}: {}",
                    err.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                ));
            }
            return Ok(m.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Wait for the event `method` on `session`; its params.
    pub(crate) fn wait_event(&mut self, session: &str, method: &str) -> Result<Value, String> {
        let is = |m: &Value| {
            m.get("method").and_then(Value::as_str) == Some(method)
                && m.get("sessionId").and_then(Value::as_str) == Some(session)
        };
        if let Some(i) = self.events.iter().position(is) {
            let m = self.events.remove(i);
            return Ok(m.get("params").cloned().unwrap_or(Value::Null));
        }
        let t0 = Instant::now();
        loop {
            let m = self.recv(t0, &format!("{method} event"))?;
            if is(&m) {
                return Ok(m.get("params").cloned().unwrap_or(Value::Null));
            }
            self.events.push(m);
        }
    }

    /// What the page `session` printed to its console and threw, in order:
    /// one line each, drained from the events read so far.
    pub(crate) fn take_console(&mut self, session: &str) -> Vec<String> {
        let mut lines = Vec::new();
        self.events.retain(|m| {
            if m.get("sessionId").and_then(Value::as_str) != Some(session) {
                return true;
            }
            match m.get("method").and_then(Value::as_str) {
                Some("Runtime.consoleAPICalled") => lines.push(console_line(m)),
                Some("Runtime.exceptionThrown") => lines.push(format!(
                    "uncaught: {}",
                    m.pointer("/params/exceptionDetails/exception/description")
                        .or_else(|| m.pointer("/params/exceptionDetails/text"))
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                )),
                _ => return true,
            }
            false
        });
        lines
    }
}

/// `console.<type>: <args>` for one `Runtime.consoleAPICalled` event.
fn console_line(m: &Value) -> String {
    let kind = m
        .pointer("/params/type")
        .and_then(Value::as_str)
        .unwrap_or("log");
    let args: Vec<String> = m
        .pointer("/params/args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| match v.get("value") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => v
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    format!("console.{kind}: {}", args.join(" "))
}

/// The value an awaited `Runtime.evaluate` answered, or the exception it
/// threw (the page's rejection, its step named).
pub(crate) fn evaluated(result: &Value) -> Result<Value, String> {
    if let Some(ex) = result.get("exceptionDetails") {
        return Err(ex
            .pointer("/exception/description")
            .or_else(|| ex.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("the page threw")
            .to_string());
    }
    result
        .pointer("/result/value")
        .cloned()
        .ok_or_else(|| "the evaluation answered no value".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_split_on_nul_and_a_partial_one_waits() {
        let mut pending = b"{\"id\":1}\0{\"method\":\"x\"}\0{\"id\"".to_vec();
        let got = split_messages(&mut pending);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].get("id").and_then(Value::as_u64), Some(1));
        assert_eq!(got[1].get("method").and_then(Value::as_str), Some("x"));
        assert_eq!(pending, b"{\"id\"");
        pending.extend_from_slice(b":2}\0");
        assert_eq!(
            split_messages(&mut pending)[0]
                .get("id")
                .and_then(Value::as_u64),
            Some(2)
        );
        assert!(pending.is_empty());
        let bad = split_messages(&mut b"nope\0".to_vec());
        assert!(bad[0].get("unparsed").is_some());
    }

    #[test]
    fn an_evaluation_answers_its_value_or_its_exception() {
        let ok = aterm_json::json!({"result": {"type": "object", "value": {"a": 1}}});
        assert_eq!(evaluated(&ok), Ok(aterm_json::json!({"a": 1})));
        let threw = aterm_json::json!({
            "result": {"type": "object"},
            "exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: init(canvas): no WebGL2"}}
        });
        assert_eq!(
            evaluated(&threw),
            Err("Error: init(canvas): no WebGL2".to_string())
        );
        assert!(evaluated(&aterm_json::json!({"result": {"type": "undefined"}})).is_err());
    }

    #[test]
    fn console_lines_name_their_kind_and_join_their_arguments() {
        let m = aterm_json::json!({"params": {"type": "error", "args": [
            {"type": "string", "value": "boom"}, {"type": "number", "value": 3},
            {"type": "object", "description": "Error: x"}
        ]}});
        assert_eq!(console_line(&m), "console.error: boom 3 Error: x");
    }

    /// The pipe round trip: a stand-in browser on two real pipes answers a
    /// call after an unrelated event, which is kept for `wait_event`, and a
    /// protocol error comes back as `Err`.
    #[test]
    fn a_call_waits_for_its_own_answer_and_keeps_the_events_between() {
        let (to_read, to_write) = std::io::pipe().expect("pipe");
        let (from_read, mut from_write) = std::io::pipe().expect("pipe");
        let browser = std::thread::spawn(move || {
            let mut r = to_read;
            let mut pending = Vec::new();
            let mut buf = [0u8; 4096];
            let mut answered = 0;
            while answered < 2 {
                let n = r.read(&mut buf).expect("read");
                pending.extend_from_slice(&buf[..n]);
                for m in split_messages(&mut pending) {
                    let id = m.get("id").and_then(Value::as_u64).expect("an id");
                    let reply = if answered == 0 {
                        format!(
                            "{{\"method\":\"Page.loadEventFired\",\"sessionId\":\"s\",\"params\":{{}}}}\0\
                             {{\"id\":{id},\"result\":{{\"ok\":true}}}}\0"
                        )
                    } else {
                        format!("{{\"id\":{id},\"error\":{{\"message\":\"no such method\"}}}}\0")
                    };
                    from_write.write_all(reply.as_bytes()).expect("write");
                    answered += 1;
                }
            }
        });
        let mut cdp = Cdp::new(to_write, from_read);
        assert_eq!(
            cdp.call(Some("s"), "Page.navigate", aterm_json::json!({}))
                .expect("answered"),
            aterm_json::json!({"ok": true})
        );
        assert_eq!(
            cdp.call(None, "Nope.nope", aterm_json::json!({})),
            Err("Nope.nope: no such method".to_string())
        );
        assert!(cdp.wait_event("s", "Page.loadEventFired").is_ok());
        browser.join().expect("the stand-in");
    }
}
