// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A sink's input writer blocked inside ConPTY's `WriteFile`, released, on a
//! real ConPTY: `Stop paste` ([`BulkMeter::request_stop`])
//! and a tab's close ([`SinkWriter::sever_input`]) reach the blocked call
//! (`CancelSynchronousIo`, `aterm_pty::input_release`), a write that begins
//! after either writes nothing, the close a stopped paste still owes waits for
//! the program to read, or for the tab to close, and a released write reports
//! exactly what the program got.
//!
//! The console host stops reading input when its output is not read: the
//! program here writes without end, nothing reads the session's output until
//! a test says so, and once the output pipe is full the host's next write of
//! output waits, holding the console. Its input thread then takes one more
//! read of input (4 KiB) and waits for the console too, so the input pipe
//! fills and the next write blocks in `WriteFile`: the writer's thread has
//! I/O pending (`GetThreadIOPendingFlag`), and the session's count of input
//! writes says which write it is. When the test reads the output, the program
//! stops writing at the first key it sees and reports every key it read, by
//! character, after a `Q`. The host takes the paste markers (`ESC [ 200 ~`,
//! `ESC [ 201 ~`) itself; the program sees the text between them.
//!
//! Each test runs under each console host this machine can start
//! ([`under_each_host`]): the Windows console host every embedder starts with,
//! and OpenConsole, the window's default; the one whose program is Git Bash
//! runs under the Windows console host, the host that gets its emoji as
//! records. The host choice is process-global, so the tests take turns.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_pty::{ConsoleHostChoice, ConsoleHostKind};
use aterm_session::sink::{BulkMeter, BulkState, SinkWriter};

/// A wait for something that must happen: a hang detector, never a budget.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// The PowerShell program: it writes without end until a key is waiting, then
/// reads keys until a `Q` and reports how many of each character it read.
const COUNTER: &str = "$f = 'x' * 2000; [Console]::Out.Write('FLOOD'); \
    while (-not [Console]::KeyAvailable) { [Console]::Out.Write($f) }; \
    [Console]::Out.Write('COUNTING'); $c = @{}; \
    while ($true) { $k = [Console]::ReadKey($true); $i = [int]$k.KeyChar; \
    if ($i -eq 81) { break }; $c[$i] = 1 + $c[$i] }; \
    [Console]::Out.Write('COUNTS ' + (($c.Keys | Sort-Object | \
    ForEach-Object { '{0}={1}' -f $_, $c[$_] }) -join ' ') + ' END')";

/// Three bytes of UTF-8: a paste of them is cut mid-character by the sink's
/// 4 KiB slices, so a stop owes the program the rest of one.
const EURO: char = '€';

/// Git for Windows' bash, a program of the MSYS2 runtime: with one in a tab,
/// the Windows console host gets each character beyond U+FFFF as its
/// surrogates' win32-input-mode records, 72 bytes for its 4 (aterm-pty's
/// `resize_guard`), whichever program reads them.
const GIT_BASH: &str = r"C:\Program Files\Git\usr\bin\bash.exe";

/// Beyond U+FFFF: with [`GIT_BASH`] in the tab, 72 bytes of records.
const GRIN: char = '😀';

/// A bracketed paste's opening and closing markers.
const OPEN: &[u8] = b"\x1b[200~";
const CLOSE: &[u8] = b"\x1b[201~";

mod k32 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetThreadIOPendingFlag(thread: isize, pending: *mut i32) -> i32;
    }
}

/// Whether `thread` has I/O pending: a `WriteFile` it is blocked in. A write
/// the pipe has room for completes in the call and never shows here.
fn io_pending<T>(thread: &JoinHandle<T>) -> bool {
    use std::os::windows::io::AsRawHandle;
    let mut pending = 0;
    // SAFETY: the join handle's thread handle is open while `thread` lives,
    // and `pending` is a valid out-param.
    let ok = unsafe { k32::GetThreadIOPendingFlag(thread.as_raw_handle() as isize, &mut pending) };
    ok != 0 && pending != 0
}

/// Wait for `what` (a hang detector, never a budget).
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < MUST_HAPPEN, "never: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Serializes the tests: each one sets the process-global console host.
static HOST: Mutex<()> = Mutex::new(());

/// Run `scenario` under the Windows console host, then under OpenConsole when
/// `auto` finds one (the window's default, the lane's Windows Terminal's;
/// without one, `auto` is the Windows console host again), handing it the
/// host each session must run in.
fn under_each_host(scenario: impl Fn(ConsoleHostKind)) {
    let _serial = HOST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for choice in [
        ConsoleHostChoice::from_config(Some("inbox")),
        ConsoleHostChoice::Auto,
    ] {
        aterm_pty::set_console_host(choice);
        scenario(aterm_pty::console_host_report().kind);
    }
    aterm_pty::set_console_host(ConsoleHostChoice::from_config(Some("inbox")));
}

/// `writer`'s answer, once it has ended (a writer the test released must end).
fn ended<T>(writer: JoinHandle<T>) -> T {
    eventually("the released writer ends", || writer.is_finished());
    writer.join().expect("the writer")
}

/// The counting program in a real ConPTY, its output unread.
struct Tab {
    master: i32,
    pid: i32,
    sink: Arc<SinkWriter>,
    output: Option<mpsc::Receiver<Vec<u8>>>,
    seen: Vec<u8>,
}

impl Tab {
    /// The counting program, in `host`, once its output fills the pipe: its
    /// host reads no more input than its one pending read.
    fn held(host: ConsoleHostKind) -> Self {
        Self::held_running(
            host,
            &[
                "powershell.exe",
                "-NoLogo",
                "-NoProfile",
                "-Command",
                COUNTER,
            ],
            false,
        )
    }

    /// `argv`, a counting program, held as [`Self::held`] holds it. With
    /// `handshake`, its first output is read as a tab's reader reads it, up to
    /// the console host's request for win32-input-mode records (which the
    /// session's guard watches the output for), on this thread, so no read of
    /// the output is left pending.
    fn held_running(host: ConsoleHostKind, argv: &[&str], handshake: bool) -> Self {
        // SAFETY: a test process; the trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let exec: Vec<std::ffi::OsString> = argv.iter().map(Into::into).collect();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&exec),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("a ConPTY spawn of the counting program");
        let master = shell.master;
        assert_eq!(
            aterm_pty::conpty_host(master),
            Some(host),
            "the session's console host"
        );
        if handshake {
            let (mut seen, mut buf) = (Vec::new(), vec![0u8; 4096]);
            eventually("the console host asks for win32-input-mode records", || {
                let n = aterm_pty::read(master, &mut buf);
                seen.extend_from_slice(&buf[..usize::try_from(n).unwrap_or(0)]);
                seen.windows(8).any(|w| w == b"\x1b[?9001h")
            });
        }
        // Nothing reads the output yet, so the peek waits behind no read.
        eventually("the program's output fills the pipe", || {
            aterm_pty::output_waiting_for_test(master).is_some_and(|n| n >= 64 * 1024)
        });
        Self {
            master,
            pid: shell.pid,
            sink: Arc::new(SinkWriter::new(master)),
            output: None,
            seen: Vec::new(),
        }
    }

    /// The input `WriteFile` calls the session has begun.
    fn writes(&self) -> u64 {
        aterm_pty::input_writes_for_test(self.master).expect("a live session")
    }

    /// Wait until `writer` is blocked in an input `WriteFile` begun after
    /// `writes` others; the host reads nothing, so it stays blocked until
    /// released.
    fn blocked<T>(&self, writer: &JoinHandle<T>, writes: u64) {
        eventually("the writer blocks in WriteFile", || {
            self.writes() > writes && io_pending(writer)
        });
    }

    /// Start reading the output: the host writes again and reads input.
    fn read_output(&mut self) {
        let (tx, rx) = mpsc::channel();
        let master = self.master;
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = aterm_pty::read(master, &mut buf);
                let Some(n) = usize::try_from(n).ok().filter(|n| *n > 0) else {
                    return;
                };
                if tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        });
        self.output = Some(rx);
    }

    /// The drawn text from the last `COUNTING` on, waiting until it holds `marker`.
    fn await_text(&mut self, marker: &str) -> String {
        let rx = self.output.as_ref().expect("reading the output");
        let start = Instant::now();
        loop {
            let text = text_of(&self.seen);
            if let Some(tail) = text.rfind("COUNTING").map(|at| &text[at..]) {
                if tail.contains(marker) {
                    return tail.to_string();
                }
            }
            // The flood is noise: keep what a marker can still be in.
            if self.seen.len() > 1 << 20 {
                let cut = self.seen.len() - (64 << 10);
                self.seen.drain(..cut);
            }
            let left = MUST_HAPPEN.saturating_sub(start.elapsed());
            match rx.recv_timeout(left) {
                Ok(chunk) => self.seen.extend_from_slice(&chunk),
                Err(e) => panic!(
                    "no {marker:?} ({e}): {:?}",
                    &text[text.len().saturating_sub(400)..]
                ),
            }
        }
    }

    /// Read the output, then every key the program read, by character, once
    /// it is told `Q`.
    fn keys_read(&mut self) -> BTreeMap<u32, u64> {
        self.read_output();
        self.await_text("COUNTING");
        assert_eq!(
            aterm_pty::write_some(self.master, b"Q").ok(),
            Some(1),
            "the Q"
        );
        let tail = self.await_text(" END");
        let counts = tail
            .split("COUNTS ")
            .nth(1)
            .and_then(|t| t.split(" END").next())
            .expect("the program's counts");
        counts
            .split_whitespace()
            .map(|pair| {
                let (c, n) = pair.split_once('=').expect("char=count");
                (c.parse().expect("a code"), n.parse().expect("a count"))
            })
            .filter(|&(_, n): &(u32, u64)| n > 0)
            .collect()
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        aterm_pty::hangup(self.pid);
        aterm_pty::reap(self.pid);
        aterm_pty::close_master(self.master);
    }
}

/// conhost draws the program's text with CSI and OSC framing; the text alone.
fn text_of(stream: &[u8]) -> String {
    let (mut text, mut i) = (Vec::new(), 0);
    while i < stream.len() {
        match (stream[i], stream.get(i + 1)) {
            (0x1b, Some(b'[')) => {
                i += 2;
                while i < stream.len() && !(0x40..=0x7e).contains(&stream[i]) {
                    i += 1;
                }
            }
            (0x1b, Some(b']')) => {
                i += 2;
                while i < stream.len() && !matches!(stream[i], 0x07 | 0x1b) {
                    i += 1;
                }
                if stream.get(i) == Some(&0x1b) {
                    i += 1;
                }
            }
            (b, _) => text.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&text).into_owned()
}

/// A bracketed paste of `n` copies of `c`.
fn paste_of(c: char, n: usize) -> Vec<u8> {
    [OPEN, c.to_string().repeat(n).as_bytes(), CLOSE].concat()
}

/// A metered paste written on its own thread, as the session's ordered
/// writer writes one: its receipt's accepted count, or the error.
fn paste_on_a_thread(
    tab: &Tab,
    frame: Vec<u8>,
    meter: &Arc<BulkMeter>,
) -> JoinHandle<Result<u64, String>> {
    let (sink, meter) = (Arc::clone(&tab.sink), Arc::clone(meter));
    std::thread::spawn(move || {
        sink.write_frame_metered_with_receipt(&frame, &meter)
            .map(|r| r.accepted() as u64)
            .map_err(|e| format!("{e:?}"))
    })
}

/// `Stop paste` into a program whose console host has stopped reading: the
/// paste's writer, blocked in ConPTY's `WriteFile`, is released; nothing more
/// of the paste is written; what the stop owes (the rest of the `€` it cut,
/// then `ESC [ 201 ~`) is the one write that follows, and it blocks as the
/// next key would, until the program reads, then lands. The program reads
/// every `€` sent before the stop, whole, and nothing after it.
#[test]
fn stop_paste_releases_a_paste_blocked_in_writefile_and_its_close_waits_for_the_program() {
    under_each_host(stop_paste_releases_a_blocked_paste);
}

fn stop_paste_releases_a_blocked_paste(host: ConsoleHostKind) {
    let mut tab = Tab::held(host);
    let meter = Arc::new(BulkMeter::new());
    let writer = paste_on_a_thread(&tab, paste_of(EURO, 100_000), &meter);
    tab.blocked(&writer, 0);
    let (writes, sent) = (tab.writes(), meter.progress().sent);
    meter.request_stop();
    tab.blocked(&writer, writes);
    assert_eq!(
        tab.writes(),
        writes + 1,
        "one write since the stop, the close it owes: the blocked one was released"
    );
    assert_eq!(meter.progress().sent, sent, "nothing more of the paste");
    let euro = EURO.len_utf8() as u64;
    let rest = (euro - (sent - OPEN.len() as u64) % euro) % euro;
    let keys = tab.keys_read();
    let accepted = ended(writer).expect("a receipt");
    assert_eq!(
        accepted,
        sent + rest + CLOSE.len() as u64,
        "the paste up to the stop, the rest of its last character, the close"
    );
    assert_eq!(meter.progress().state, BulkState::Stopped);
    assert_eq!(
        keys,
        BTreeMap::from([(u32::from(EURO), (sent + rest - OPEN.len() as u64) / euro)]),
        "every € sent before the stop, whole, and nothing after it"
    );
}

/// `Stop paste` of emoji into a tab with Git Bash in it, under the Windows
/// console host, where each goes as 72 bytes of records, so a 4 KiB slice of
/// the paste is a wire larger than the pipe: every write ends between
/// characters, no larger than the pipe, and the one the stop releases took
/// none of them. The paste ends `Stopped`, not failed, owing only
/// `ESC [ 201 ~`, and the program reads exactly the characters its receipt
/// counts. The reader is the counting PowerShell bash runs (`exec`, which
/// keeps bash's process as its parent): MSYS2's own console reading takes
/// records off the queue and writes them back (`resize_guard`'s module docs),
/// and a bash reading this paste once lost two records and reordered others,
/// which is the runtime's, not the writes'.
#[test]
fn stop_paste_of_records_releases_whole_characters_the_receipt_counts() {
    let _serial = HOST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    aterm_pty::set_console_host(ConsoleHostChoice::from_config(Some("inbox")));
    // Own the entire fixture directory rather than adopting a PID-named file.
    // Spaces and an apostrophe also exercise the Bash argument's quoting.
    let script_dir = aterm_tempfile::Builder::new()
        .prefix("aterm counter's ")
        .tempdir()
        .expect("the counter's exclusive directory");
    let script = script_dir.path().join("counter.ps1");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&script)
        .and_then(|mut file| file.write_all(COUNTER.as_bytes()))
        .expect("the counter's script");
    let quoted_path = script
        .to_str()
        .expect("a UTF-8 counter path for Git Bash")
        .replace('\\', "/")
        .replace('\'', "'\\''");
    let run = format!(
        "exec powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File '{quoted_path}'"
    );
    let mut tab = Tab::held_running(ConsoleHostKind::Inbox, &[GIT_BASH, "-c", &run], true);
    let meter = Arc::new(BulkMeter::new());
    let writer = paste_on_a_thread(&tab, paste_of(GRIN, 100_000), &meter);
    tab.blocked(&writer, 0);
    let (writes, sent) = (tab.writes(), meter.progress().sent);
    let grin = GRIN.len_utf8() as u64;
    let open = OPEN.len() as u64;
    assert_eq!(
        sent,
        open + grin * ((64 * 1024 - open) / 72),
        "the first write: as many characters' records as the pipe's quota holds"
    );
    meter.request_stop();
    // The released write needed more room than the pipe had left; the
    // close's six bytes fit it, so the close does not wait for the program.
    let accepted = ended(writer).expect("a receipt, not a failed paste");
    assert_eq!(
        tab.writes(),
        writes + 1,
        "one write since the stop, the close it owes: the blocked one was released"
    );
    assert_eq!(
        accepted,
        sent + CLOSE.len() as u64,
        "the paste up to the stop, whole characters, then the close"
    );
    assert_eq!(meter.progress().state, BulkState::Stopped);
    let keys = tab.keys_read();
    let mut units = [0u16; 2];
    let [high, low] = GRIN.encode_utf16(&mut units) else {
        unreachable!("a character beyond U+FFFF is a surrogate pair")
    };
    let each = (sent - open) / grin;
    assert_eq!(
        keys,
        BTreeMap::from([(u32::from(*high), each), (u32::from(*low), each)]),
        "every character the receipt counts, by its surrogates, and nothing else"
    );
    drop(tab);
}

/// A tab's close releases the close a stopped paste still owes, blocked in
/// ConPTY's `WriteFile` for a program that does not read, and every write
/// after the close writes nothing: no `WriteFile` is made, through the sink
/// or under it.
#[test]
fn a_tabs_close_releases_the_owed_close_and_bars_every_later_write() {
    under_each_host(a_close_releases_the_owed_close);
}

fn a_close_releases_the_owed_close(host: ConsoleHostKind) {
    let tab = Tab::held(host);
    let meter = Arc::new(BulkMeter::new());
    let writer = paste_on_a_thread(&tab, paste_of(EURO, 100_000), &meter);
    tab.blocked(&writer, 0);
    let (writes, sent) = (tab.writes(), meter.progress().sent);
    meter.request_stop();
    tab.blocked(&writer, writes);
    tab.sink.sever_input();
    let accepted = ended(writer).expect("a receipt");
    assert_eq!(accepted, sent, "the close was not written");
    assert_eq!(meter.progress().state, BulkState::Stopped);
    assert!(
        tab.sink.write_frame(b"x").is_err(),
        "the sink refuses a write after the close"
    );
    assert_eq!(
        aterm_pty::write_some(tab.master, b"x").ok(),
        Some(0),
        "the session writes nothing"
    );
    assert_eq!(tab.writes(), writes + 1, "and makes no WriteFile");
}

/// A tab's close releases a plain (unmetered) frame's writer blocked in
/// ConPTY's `WriteFile`; the frame ends there, and nothing more is written.
#[test]
fn a_tabs_close_releases_a_frame_blocked_in_writefile() {
    under_each_host(a_close_releases_a_blocked_frame);
}

fn a_close_releases_a_blocked_frame(host: ConsoleHostKind) {
    let tab = Tab::held(host);
    let total = 256 * 1024;
    let writer = {
        let sink = Arc::clone(&tab.sink);
        std::thread::spawn(move || {
            sink.write_frame_with_receipt(&vec![b'a'; total])
                .map(|r| r.accepted())
                .map_err(|e| format!("{e:?}"))
        })
    };
    tab.blocked(&writer, 0);
    let writes = tab.writes();
    tab.sink.sever_input();
    let accepted = ended(writer).expect("a receipt");
    assert!(accepted < total, "the frame ended at the close: {accepted}");
    assert_eq!(tab.writes(), writes, "nothing written after the close");
}

/// A write released in ConPTY's `WriteFile` reports what the program got:
/// writes of a frame larger than the pipe, one after another as the sink's
/// Windows body makes them, until a stop releases the blocked one; the program
/// then reads exactly the bytes the writes reported. (A `WriteFile` larger
/// than the pipe, cancelled after the host read part of it, reports nothing
/// written, so `aterm_pty` hands the pipe at most its 64 KiB at a time.) A
/// write that begins after the stop makes no `WriteFile`.
#[test]
fn a_released_write_reports_every_byte_the_program_reads() {
    under_each_host(a_released_write_reports_what_was_read);
}

fn a_released_write_reports_what_was_read(host: ConsoleHostKind) {
    let mut tab = Tab::held(host);
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (m, stop) = (tab.master, Arc::clone(&stop));
        std::thread::spawn(move || {
            let frame = vec![b'a'; 256 * 1024];
            let mut reported = 0u64;
            loop {
                match aterm_pty::write_some_stoppable(m, &frame, Some(&stop)) {
                    Ok(0) => return Ok(reported),
                    Ok(n) => reported += n as u64,
                    Err(e) => return Err(e.to_string()),
                }
            }
        })
    };
    tab.blocked(&writer, 0);
    stop.store(true, Ordering::Release);
    aterm_pty::release_stopped_writes(&stop);
    let reported = ended(writer).expect("the writes");
    let writes = tab.writes();
    assert_eq!(
        aterm_pty::write_some_stoppable(tab.master, b"x", Some(&stop)).ok(),
        Some(0),
        "a write of the stopped frame writes nothing"
    );
    assert_eq!(tab.writes(), writes, "and makes no WriteFile");
    assert_eq!(
        tab.keys_read(),
        BTreeMap::from([(u32::from('a'), reported)]),
        "the program read what the writes reported"
    );
}
