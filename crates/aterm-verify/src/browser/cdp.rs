// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Chrome DevTools protocol over `--remote-debugging-pipe` (fds 3 and 4, NUL-delimited JSON).

use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use aterm_json::Value;

use crate::browser::serve;

/// How long any one answer or event may take before the lane calls it hung: from the call, from
/// its page's last beat where the call waits on one ([`Cdp::call_beating`]), or from the page's
/// last message where the wait is for a console line ([`Cdp::wait_console`]).
pub(crate) const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// How [`Cdp::take_console`] begins the line of an exception nobody caught.
pub(crate) const UNCAUGHT: &str = "uncaught: ";

/// One DevTools connection.
pub(crate) struct Cdp {
    to: std::io::PipeWriter,
    from: Receiver<Value>,
    next: u64,
    /// Events read while waiting for something else, oldest first.
    events: Vec<Value>,
}

/// The NUL-ended messages in `pending`, parsed and drained; a partial message stays.
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
    /// A connection over the two pipe ends (`to` is the browser's fd 3, `from` its fd 4).
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

    /// The next message, waited for until [`serve::hang_at`] of `t0` and the last of `beats` that
    /// came after it.
    fn recv(
        &mut self,
        t0: Instant,
        what: &str,
        beats: Option<&serve::Beats>,
    ) -> Result<Value, String> {
        loop {
            let last = beats.and_then(serve::Beats::last);
            let wait = serve::wait_step(t0, last, Instant::now(), what)?;
            match self.from.recv_timeout(wait) {
                Ok(m) => return Ok(m),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "the browser closed its DevTools pipe before {what}"
                    ));
                }
            }
        }
    }

    /// Sends `method` with `params` and waits for its answer (`result` or protocol error).
    pub(crate) fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.call_until(session, method, params, None)
    }

    /// [`Self::call`] for a call whose page beats while it works: hung only once a minute passes
    /// with no answer and no beat ([`serve::hang_at`]).
    pub(crate) fn call_beating(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        beats: &serve::Beats,
    ) -> Result<Value, String> {
        self.call_until(session, method, params, Some(beats))
    }

    fn call_until(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        beats: Option<&serve::Beats>,
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
            let m = self.recv(t0, &format!("answer to {method}"), beats)?;
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
            let m = self.recv(t0, &format!("{method} event"), None)?;
            if is(&m) {
                return Ok(m.get("params").cloned().unwrap_or(Value::Null));
            }
            self.events.push(m);
        }
    }

    /// Waits for console line `marker` from `session`, resetting the hang detector on each message
    /// the page sends.
    pub(crate) fn wait_console(&mut self, session: &str, marker: &str) -> Result<(), String> {
        let from_page = |m: &Value| m.get("sessionId").and_then(Value::as_str) == Some(session);
        let is = |m: &Value| {
            from_page(m)
                && m.get("method").and_then(Value::as_str) == Some("Runtime.consoleAPICalled")
                && m.pointer("/params/args/0/value").and_then(Value::as_str) == Some(marker)
        };
        let crashed = |m: &Value| {
            from_page(m)
                && m.get("method").and_then(Value::as_str) == Some("Inspector.targetCrashed")
        };
        let crash = || format!("the page crashed before {marker:?}");
        if let Some(i) = self.events.iter().position(|m| is(m) || crashed(m)) {
            if crashed(&self.events[i]) {
                return Err(crash());
            }
            self.events.remove(i);
            return Ok(());
        }
        let mut t0 = Instant::now();
        loop {
            let m = self.recv(t0, &format!("console line {marker:?}"), None)?;
            if is(&m) {
                return Ok(());
            }
            if crashed(&m) {
                return Err(crash());
            }
            if from_page(&m) {
                t0 = Instant::now();
            }
            self.events.push(m);
        }
    }

    /// Drains console and exception lines for `session` from the events read so far.
    pub(crate) fn take_console(&mut self, session: &str) -> Vec<String> {
        let mut lines = Vec::new();
        self.events.retain(|m| {
            if m.get("sessionId").and_then(Value::as_str) != Some(session) {
                return true;
            }
            match m.get("method").and_then(Value::as_str) {
                Some("Runtime.consoleAPICalled") => lines.push(console_line(m)),
                Some("Runtime.exceptionThrown") => lines.push(format!(
                    "{UNCAUGHT}{}",
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

/// The value an awaited `Runtime.evaluate` answered, or the exception it threw.
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

    #[test]
    fn a_console_marker_ends_the_wait_and_the_lines_before_it_stay() {
        let (_to_read, to_write) = std::io::pipe().expect("pipe");
        let (from_read, mut from_write) = std::io::pipe().expect("pipe");
        let line = |session: &str, text: &str| {
            format!(
                "{{\"method\":\"Runtime.consoleAPICalled\",\"sessionId\":\"{session}\",\"params\":\
                 {{\"type\":\"error\",\"args\":[{{\"type\":\"string\",\"value\":\"{text}\"}}]}}}}\0"
            )
        };
        let mut sent = line("s", "first");
        sent.push_str(&line("other", "done"));
        sent.push_str(&line("s", "second"));
        sent.push_str(&line("s", "done"));
        sent.push_str(
            "{\"method\":\"Inspector.targetCrashed\",\"sessionId\":\"s\",\"params\":{}}\0",
        );
        from_write.write_all(sent.as_bytes()).expect("write");
        let mut cdp = Cdp::new(to_write, from_read);
        assert_eq!(cdp.wait_console("s", "done"), Ok(()));
        assert_eq!(
            cdp.take_console("s"),
            ["console.error: first", "console.error: second"]
        );
        assert_eq!(
            cdp.wait_console("s", "done"),
            Err("the page crashed before \"done\"".to_string())
        );
        assert_eq!(cdp.wait_console("other", "done"), Ok(()));
    }

    #[test]
    fn a_crash_read_during_an_earlier_call_ends_the_console_wait_at_once() {
        let (to_read, to_write) = std::io::pipe().expect("pipe");
        let (from_read, mut from_write) = std::io::pipe().expect("pipe");
        let crashed = |session: &str| {
            format!(
                "{{\"method\":\"Inspector.targetCrashed\",\"sessionId\":\"{session}\",\"params\":{{}}}}\0"
            )
        };
        let marker = |session: &str| {
            format!(
                "{{\"method\":\"Runtime.consoleAPICalled\",\"sessionId\":\"{session}\",\"params\":\
                 {{\"type\":\"debug\",\"args\":[{{\"type\":\"string\",\"value\":\"done\"}}]}}}}\0"
            )
        };
        let mut sent = crashed("other");
        sent.push_str(&marker("early"));
        sent.push_str(&crashed("early"));
        sent.push_str(&crashed("s"));
        sent.push_str("{\"id\":1,\"result\":{}}\0");
        from_write.write_all(sent.as_bytes()).expect("write");
        let mut cdp = Cdp::new(to_write, from_read);
        cdp.call(Some("s"), "Runtime.evaluate", aterm_json::json!({}))
            .expect("answered");
        drop(to_read);
        assert_eq!(
            cdp.wait_console("s", "done"),
            Err("the page crashed before \"done\"".to_string())
        );
        assert_eq!(cdp.wait_console("early", "done"), Ok(()));
        drop(from_write);
    }
}
