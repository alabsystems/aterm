// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frame stream recorder and harness journal tailer on a shared wall clock.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::door::Door;
use crate::pyjson::{J, Style};
use crate::screen::{composer_of, has_card};
use crate::stop::Stopped;
use crate::util::now;

const LINE: Style = Style {
    indent: None,
    compact: false,
    ascii: false,
};

/// One screen as the stream gave it.
#[derive(Clone, Debug)]
pub struct Frame {
    /// When it was read (seconds since the epoch).
    pub wall: f64,
    /// The server's video clock (microseconds) at the last `T` line.
    pub t: Option<i64>,
    /// The content sequence.
    pub seq: i64,
    /// Its rows (trimmed).
    pub rows: Vec<String>,
    /// Its index in the recording.
    pub i: usize,
}

impl Frame {
    /// Whether the survey card shows.
    #[must_use]
    pub fn card(&self) -> bool {
        has_card(&self.rows)
    }

    /// The composer's first row.
    #[must_use]
    pub fn comp(&self) -> Option<String> {
        composer_of(&self.rows)
    }
}

/// An EVENT line of the stream: (wall, T, the line).
pub type Event = (f64, Option<i64>, String);

#[derive(Default)]
struct Shared {
    frames: Vec<Frame>,
    events: Vec<Event>,
    err: String,
}

/// The recorder of one lab tab's frame stream.
pub struct Recorder {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    stop: Arc<AtomicBool>,
    stream: Option<aterm_uds::CtlStream>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Parse a stream into frames and events (`T`, `DELTA … screen N` + N rows, `EVENT`).
pub struct StreamParser {
    t: Option<i64>,
    pending: Option<(i64, usize, Vec<String>)>,
}

/// What one line of the stream completed.
#[derive(Debug, PartialEq)]
pub enum Item {
    /// A frame: (seq, its rows).
    Frame(i64, Vec<String>),
    /// An EVENT line.
    Event(String),
}

impl Default for StreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamParser {
    /// A parser at the start of a stream.
    #[must_use]
    pub const fn new() -> Self {
        StreamParser {
            t: None,
            pending: None,
        }
    }

    /// The video clock as of the last `T` line.
    #[must_use]
    pub const fn t(&self) -> Option<i64> {
        self.t
    }

    /// Feed one line; what it completed.
    pub fn line(&mut self, line: &str) -> Option<Item> {
        if let Some((seq, n, mut rows)) = self.pending.take() {
            rows.push(line.to_string());
            if rows.len() == n {
                return Some(Item::Frame(seq, rows));
            }
            self.pending = Some((seq, n, rows));
            return None;
        }
        if let Some(rest) = line.strip_prefix("T ") {
            if let Some(t) = rest.split_whitespace().nth(1).and_then(|v| v.parse().ok()) {
                self.t = Some(t);
            }
        } else if line.starts_with("DELTA ") && line.contains(" screen ") {
            let w: Vec<&str> = line.split_whitespace().collect();
            let seq = w
                .iter()
                .find_map(|x| x.strip_prefix("seq="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let n: usize = w
                .iter()
                .position(|x| *x == "screen")
                .and_then(|i| w.get(i + 1))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if n == 0 {
                return Some(Item::Frame(seq, Vec::new()));
            }
            self.pending = Some((seq, n, Vec::with_capacity(n)));
        } else if line.starts_with("EVENT ") {
            return Some(Item::Event(line.to_string()));
        }
        None
    }
}

impl Recorder {
    /// Subscribes to tab `sid` on `door`, optionally writing to `<out>/frames.jsonl`.
    ///
    /// # Errors
    /// The connection or the subscription failed.
    pub fn start(door: &Door<'_>, sid: &str, out: Option<&Path>) -> Result<Recorder, String> {
        let mut client = door.connect()?;
        let stream = client.transport().try_clone().map_err(|e| e.to_string())?;
        let reply = client
            .request_line(&format!(
                "subscribe @{sid} screen,trim,ts,events every-frame"
            ))
            .map_err(|e| format!("subscribe: {e}"))?;
        if reply != "OK" && !reply.starts_with("OK ") {
            return Err(format!("subscribe: {reply}"));
        }
        let shared: Arc<(Mutex<Shared>, Condvar)> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let mut file = out.and_then(|d| std::fs::File::create(d.join("frames.jsonl")).ok());
        let (sh, st) = (Arc::clone(&shared), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            let mut p = StreamParser::new();
            loop {
                let line = match client.next_pushed() {
                    Ok(l) => l,
                    Err(e) => {
                        if !st.load(Ordering::SeqCst) {
                            sh.0.lock().unwrap_or_else(|x| x.into_inner()).err = e.to_string();
                        }
                        sh.1.notify_all();
                        return;
                    }
                };
                let wall = now();
                let item = p.line(&line);
                let (lock, cv) = &*sh;
                match item {
                    Some(Item::Frame(seq, rows)) => {
                        if let Some(f) = file.as_mut() {
                            let rec = J::obj([
                                ("wall", J::Float(wall)),
                                ("T", J::from(p.t())),
                                ("seq", J::Int(seq)),
                                (
                                    "rows",
                                    J::Arr(rows.iter().map(|r| J::s(r.as_str())).collect()),
                                ),
                            ]);
                            let _ = writeln!(f, "{}", rec.dump(LINE));
                        }
                        let mut g = lock.lock().unwrap_or_else(|x| x.into_inner());
                        let i = g.frames.len();
                        g.frames.push(Frame {
                            wall,
                            t: p.t(),
                            seq,
                            rows,
                            i,
                        });
                        cv.notify_all();
                    }
                    Some(Item::Event(e)) => {
                        if let Some(f) = file.as_mut() {
                            let rec = J::obj([
                                ("wall", J::Float(wall)),
                                ("T", J::from(p.t())),
                                ("event", J::s(e.as_str())),
                            ]);
                            let _ = writeln!(f, "{}", rec.dump(LINE));
                        }
                        lock.lock().unwrap_or_else(|x| x.into_inner()).events.push((
                            wall,
                            p.t(),
                            e,
                        ));
                        cv.notify_all();
                    }
                    None => {}
                }
            }
        });
        Ok(Recorder {
            shared,
            stop,
            stream: Some(stream),
            thread: Some(thread),
        })
    }

    /// End the subscription.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = self.stream.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// What ended the stream when nobody asked it to (empty otherwise).
    #[must_use]
    pub fn err(&self) -> String {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|x| x.into_inner())
            .err
            .clone()
    }

    /// The newest frame.
    #[must_use]
    pub fn last(&self) -> Option<Frame> {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|x| x.into_inner())
            .frames
            .last()
            .cloned()
    }

    /// How many frames so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|x| x.into_inner())
            .frames
            .len()
    }

    /// Every frame and event so far.
    #[must_use]
    pub fn snapshot(&self) -> (Vec<Frame>, Vec<Event>) {
        let g = self.shared.0.lock().unwrap_or_else(|x| x.into_inner());
        (g.frames.clone(), g.events.clone())
    }

    /// The frames from index `since` on.
    #[must_use]
    pub fn frames_from(&self, since: usize) -> Vec<Frame> {
        let g = self.shared.0.lock().unwrap_or_else(|x| x.into_inner());
        g.frames
            .get(since..)
            .map(<[Frame]>::to_vec)
            .unwrap_or_default()
    }

    /// Waits up to `timeout` seconds for a frame at or after `since` matching `pred`.
    ///
    /// # Errors
    /// The run was interrupted.
    pub fn wait_for(
        &self,
        pred: impl Fn(&Frame) -> bool,
        timeout: f64,
        since: usize,
    ) -> Result<Option<Frame>, Stopped> {
        let end = now() + timeout;
        let mut i = since;
        let (lock, cv) = &*self.shared;
        let mut g = lock.lock().unwrap_or_else(|x| x.into_inner());
        loop {
            while i < g.frames.len() {
                let f = &g.frames[i];
                i += 1;
                if pred(f) {
                    return Ok(Some(f.clone()));
                }
            }
            crate::stop::check()?;
            let left = end - now();
            if left <= 0.0 {
                return Ok(None);
            }
            g = cv
                .wait_timeout(g, Duration::from_secs_f64(left.min(0.25)))
                .unwrap_or_else(|x| x.into_inner())
                .0;
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Tailer for the harness journal (`<state>/drive/<sid>.journal.jsonl`).
pub struct Journal {
    lines: Arc<Mutex<Vec<(f64, J)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Journal {
    /// Tail `path`; with `out`, every record also goes to `<out>/journal.jsonl`.
    #[must_use]
    pub fn start(path: PathBuf, out: Option<&Path>) -> Journal {
        let lines: Arc<Mutex<Vec<(f64, J)>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let mut file = out.and_then(|d| std::fs::File::create(d.join("journal.jsonl")).ok());
        let (l, s) = (Arc::clone(&lines), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            let mut n = 0usize;
            while !s.load(Ordering::SeqCst) {
                let text = crate::sys::scratch_read(&path)
                    .ok()
                    .flatten()
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                let ls: Vec<&str> = text.lines().collect();
                if ls.len() > n {
                    let w = now();
                    for ln in &ls[n..] {
                        let rec = J::parse(ln).unwrap_or_else(|_| {
                            #[allow(clippy::cast_possible_truncation)]
                            let t = (w * 1000.0) as i64;
                            J::obj([("t", J::Int(t)), ("kind", J::s("raw")), ("line", J::s(*ln))])
                        });
                        if let Some(f) = file.as_mut() {
                            let _ = writeln!(
                                f,
                                "{}",
                                J::obj([("seen", J::Float(w)), ("rec", rec.clone())]).dump(LINE)
                            );
                        }
                        l.lock().unwrap_or_else(|x| x.into_inner()).push((w, rec));
                    }
                    n = ls.len();
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        Journal {
            lines,
            stop,
            thread: Some(thread),
        }
    }

    /// Every record so far.
    #[must_use]
    pub fn snapshot(&self) -> Vec<(f64, J)> {
        self.lines.lock().unwrap_or_else(|x| x.into_inner()).clone()
    }

    /// End the tail.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A record's `t` as seconds.
#[must_use]
pub fn rec_t(rec: &J) -> f64 {
    rec.get("t").and_then(J::as_f64).unwrap_or(0.0) / 1000.0
}

/// A record's string field.
#[must_use]
pub fn rec_s(rec: &J, key: &str) -> String {
    rec.get(key).and_then(J::as_str).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_yields_frames_with_their_rows_and_events_whole() {
        let mut p = StreamParser::new();
        let lines = [
            "sub 0 s-168a1e0595c70920336a",
            "T 0 9952521",
            "DELTA 0 seq=8 screen 1",
            "lab$",
            "DELTA 0 seq=19 screen 3",
            "lab$ echo hi",
            "hi",
            "lab$",
            "EVENT 0 block-complete 0 exit=0",
        ];
        let items: Vec<Item> = lines.iter().filter_map(|l| p.line(l)).collect();
        assert_eq!(
            items,
            vec![
                Item::Frame(8, vec!["lab$".into()]),
                Item::Frame(19, vec!["lab$ echo hi".into(), "hi".into(), "lab$".into()]),
                Item::Event("EVENT 0 block-complete 0 exit=0".into()),
            ]
        );
        assert_eq!(p.t(), Some(9_952_521));
    }
}
