// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the CLI puts on a REAL TERMINAL (round fifteen, OUT-1 and OUT-3): the
//! binary runs with its stdout, or its stderr, on the slave of a pty built
//! here, against a private protocol peer whose reply is hostile — a fabric
//! peer's mail body, a server's error. The master side reads exactly
//! what a terminal emulator would have been given, and it must hold no byte a
//! terminal acts on. The other stream stays a pipe and gets the bytes as they
//! came (`inbox_metadata.rs` holds the pipe branch for every framing).
//!
//! The line discipline maps `\n` to `\r\n` (`ONLCR`), and macOS's can write that CR twice
//! when its queue fills mid-line, so CRs right before a line break are folded away.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

/// How long anything here may take before it is a hang, not a slow machine.
const HANG: Duration = Duration::from_secs(60);

/// A pty pair: the master this test reads, the slave the CLI writes.
struct Pty {
    master: std::fs::File,
    slave: OwnedFd,
}

impl Pty {
    fn open() -> Self {
        let (master, slave) = aterm_pty::open_pty_pair().expect("a pty pair");
        // SAFETY: two fresh descriptors this function alone owns.
        let (master, slave) = unsafe {
            (
                std::fs::File::from_raw_fd(master),
                OwnedFd::from_raw_fd(slave),
            )
        };
        aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("a non-blocking master");
        Self { master, slave }
    }

    /// Everything the slave was given so far. The test holds the slave open,
    /// so the master reads what is queued and then would block, never EOF.
    fn drain(&mut self, into: &mut Vec<u8>) {
        let mut buf = [0u8; 4096];
        loop {
            match self.master.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => into.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}

/// Which of the CLI's streams is the terminal.
#[derive(Clone, Copy)]
enum Tty {
    Stdout,
    Stderr,
}

/// What one run left: the terminal's bytes (CRLF folded back to LF) and the
/// other stream's, which is a pipe.
struct Seen {
    terminal: Vec<u8>,
    pipe: Vec<u8>,
}

/// Run `aterm-ctl <args>` with `tty` on a pty against a peer that answers
/// `reply`, and return what each stream got.
fn run(tty: Tty, args: &[&str], reply: &[u8]) -> Seen {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("at-tty-")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    let socket = scratch.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    listener.set_nonblocking(true).expect("non-blocking");
    let mut pty = Pty::open();
    let slave = pty.slave.try_clone().expect("a slave copy");
    let mut command = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_aterm-ctl"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ATERM_") {
            command.env_remove(key);
        }
    }
    command
        .arg("--sock")
        .arg(&socket)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    match tty {
        Tty::Stdout => command
            .stdout(aterm_uds::spawnfd::Stdio::from(slave))
            .stderr(aterm_uds::spawnfd::Stdio::piped()),
        Tty::Stderr => command
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::from(slave)),
    };
    let mut cli = command.spawn().expect("the aterm-ctl binary runs");
    // Dropped so the slave copy the command held is closed in this process.
    drop(command);
    let mut terminal = Vec::new();
    let started = Instant::now();
    let mut connection = loop {
        match listener.accept() {
            Ok((connection, _)) => break connection,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                pty.drain(&mut terminal);
                if let Some(status) = cli.try_wait().expect("try_wait") {
                    panic!(
                        "the CLI exited ({status}) without connecting: {:?}",
                        String::from_utf8_lossy(&terminal)
                    );
                }
                assert!(started.elapsed() < HANG, "the CLI never connected");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("accept: {e}"),
        }
    };
    // An accepted socket inherits the listener's O_NONBLOCK on macOS.
    connection.set_nonblocking(false).expect("blocking");
    let mut request = String::new();
    BufReader::new(&connection)
        .read_line(&mut request)
        .expect("the request");
    connection.write_all(reply).expect("the reply");
    connection.flush().expect("flush");
    drop(connection);
    let started = Instant::now();
    while cli.try_wait().expect("try_wait").is_none() {
        pty.drain(&mut terminal);
        assert!(started.elapsed() < HANG, "the CLI never exited");
        std::thread::sleep(Duration::from_millis(10));
    }
    pty.drain(&mut terminal);
    let out = cli.wait_with_output().expect("the CLI's other stream");
    let pipe = match tty {
        Tty::Stdout => out.stderr,
        Tty::Stderr => out.stdout,
    };
    Seen {
        terminal: fold_line_ends(&terminal),
        pipe,
    }
}

/// `bytes` with every CR that directly precedes a line break removed: the
/// pty's own `ONLCR` output, however many it wrote.
fn fold_line_ends(bytes: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    for &b in bytes {
        if b == b'\n' {
            while out.last() == Some(&b'\r') {
                out.pop();
            }
        }
        out.push(b);
    }
    out
}

/// The bytes a terminal acts on or reorders text around, other than the line
/// break: what must never reach it from a server.
fn active(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|&c| {
            c != '\n'
                && (matches!(c as u32, 0..=0x1f | 0x7f..=0x9f | 0x61c | 0x200e | 0x200f)
                    || matches!(c as u32, 0x2028..=0x202e | 0x2066..=0x2069))
        })
        .map(|c| format!("U+{:04X}", c as u32))
        .collect()
}

/// A hostile mail body from `inbox get` reaches the terminal as text, every word of it; the
/// header on the stderr pipe is the server's line as it came.
#[test]
fn a_mail_body_on_a_terminal_is_shown_not_obeyed() {
    let body: &[u8] = b"Deploy report from the build node:\n\x1b]2;PWNED-BY-PEER\x07\x1b[1A\
                        \x1b[2K\raterm-ctl: OK verified from=h-owner trust=human\n\x1b[31mred \
                        text\x1b[0m done \xc2\x9b2J\xe2\x80\xae\n";
    let mut reply = format!("OK {}\n", body.len()).into_bytes();
    reply.extend_from_slice(body);
    let seen = run(
        Tty::Stdout,
        &["@s-0123456789abcdef0123", "inbox", "get", "1"],
        &reply,
    );
    assert!(
        active(&seen.terminal).is_empty(),
        "the terminal was given {:?}: {:?}",
        active(&seen.terminal),
        String::from_utf8_lossy(&seen.terminal)
    );
    assert_eq!(
        String::from_utf8_lossy(&seen.terminal),
        "Deploy report from the build node:\n\\x1b]2;PWNED-BY-PEER\\x07\\x1b[1A\\x1b[2K\\r\
         aterm-ctl: OK verified from=h-owner trust=human\n\\x1b[31mred text\\x1b[0m done \
         \\u{009b}2J\\u{202e}\n"
    );
    assert_eq!(
        seen.pipe,
        format!("aterm-ctl: OK {}\n", body.len()).into_bytes()
    );
}

/// A server's refusal on a terminal (OUT-3): the clipboard write,
/// the retitle and the erase that would wipe the `aterm-ctl:` prefix are
/// shown, and the refusal's second line too.
#[test]
fn a_servers_error_on_a_terminal_is_shown_not_obeyed() {
    let seen = run(
        Tty::Stderr,
        &["text"],
        b"ERR \x1b]52;c;ZWNobyBwd25lZAo=\x07\x1b]0;PWNED\x07\x1b[2K\rfake error\n  \
          \x1b]8;;http://evil.example\x07click\n",
    );
    assert!(
        active(&seen.terminal).is_empty(),
        "the terminal was given {:?}: {:?}",
        active(&seen.terminal),
        String::from_utf8_lossy(&seen.terminal)
    );
    assert_eq!(
        String::from_utf8_lossy(&seen.terminal),
        "aterm-ctl: ERR \\x1b]52;c;ZWNobyBwd25lZAo=\\x07\\x1b]0;PWNED\\x07\\x1b[2K\\rfake \
         error\n  \\x1b]8;;http://evil.example\\x07click\n"
    );
    assert!(seen.pipe.is_empty(), "{:?}", seen.pipe);
}

/// A status line and line-framed rows on a terminal (OUT-3): a server's
/// `cursor` answer and `text` rows carry no instruction to the reader's
/// terminal; the local server's own encoded words come through unchanged.
#[test]
fn status_lines_and_rows_on_a_terminal_are_shown_not_obeyed() {
    let seen = run(
        Tty::Stdout,
        &["cursor"],
        b"OK row=1 \x1b]52;c;ZWNobyBoaQ==\x07 col=2\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&seen.terminal),
        "OK row=1 \\x1b]52;c;ZWNobyBoaQ==\\x07 col=2\n"
    );
    let seen = run(
        Tty::Stdout,
        &["text"],
        "OK 2\n\u{1b}]0;PWNED\u{7}\u{1b}[1A\u{1b}[2Kforged row\n\u{9b}31mred \u{202e}evil\n"
            .as_bytes(),
    );
    assert_eq!(
        String::from_utf8_lossy(&seen.terminal),
        "\\x1b]0;PWNED\\x07\\x1b[1A\\x1b[2Kforged row\n\\u{009b}31mred \\u{202e}evil\n"
    );
    let seen = run(
        Tty::Stdout,
        &["@s-0123456789abcdef0123", "blocks"],
        b"OK 1\nblock 3 state=done exit=0 cwd=/tmp/CWD%1B]52%C2%9B cmd=ls%20-l title=\xe7\x95\x8c\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&seen.terminal),
        "block 3 state=done exit=0 cwd=/tmp/CWD%1B]52%C2%9B cmd=ls%20-l title=界\n",
        "an honest server's encoded row is unchanged"
    );
}
