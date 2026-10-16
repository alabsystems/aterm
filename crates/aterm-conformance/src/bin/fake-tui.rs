// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `fake-tui --shape <claude|streamer|cold-streamer|repainter>`: the stand-in
//! programs the paint and spin scenes run inside a headless aterm. Each writes
//! one byte choreography a real program writes, and nothing else; Ctrl-C quits
//! every shape.
//!
//! * `claude`: alt screen; every repaint hides the caret inside a DEC-2026
//!   bracket; a spinner line repaints every 150 ms on its own; each typed key
//!   rewrites the input row. Typed into, it is the fake-Claude scene; left
//!   alone, the dark control.
//! * `streamer`: alt screen; token chunks land at row 1 several times a second
//!   through ESC 7 / CUP / ESC 8, away from a caret that echoes each key.
//! * `cold-streamer`: alt screen; an append-shaped stream walks a shown caret
//!   (desaturated over OSC 12, so the caret itself is not ink) one cell at a
//!   time, folding and scrolling; the alphabet is nonsense so no word
//!   decoration fires.
//! * `repainter`: one second of real writes, then a DEC-2026 bracket around a
//!   bare cursor position ~6x/s: output that marks no cell, so the session
//!   stays Running while the grid's movement clock goes stale.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if args == ["--version"] {
        println!("fake-tui {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    #[cfg(unix)]
    let ran = shapes::run(&args);
    #[cfg(not(unix))]
    let ran = false;
    if !ran {
        eprintln!("usage: fake-tui --shape <claude|streamer|cold-streamer|repainter> (unix)");
        std::process::exit(2);
    }
}

#[cfg(unix)]
mod shapes {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Write and flush at once: each call is one batch the terminal parses.
    fn w(bytes: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes.as_bytes());
        let _ = out.flush();
    }

    /// The terminal in raw mode (no echo, no line discipline) until dropped.
    struct Raw(libc::termios);

    impl Raw {
        fn enter() -> Self {
            // SAFETY: a zeroed termios is a valid out-parameter for tcgetattr on
            // fd 0, and cfmakeraw/tcsetattr only read and write that struct.
            unsafe {
                let mut saved: libc::termios = std::mem::zeroed();
                libc::tcgetattr(0, &raw mut saved);
                let mut raw = saved;
                libc::cfmakeraw(&raw mut raw);
                libc::tcsetattr(0, libc::TCSAFLUSH, &raw const raw);
                Self(saved)
            }
        }
    }

    impl Drop for Raw {
        fn drop(&mut self) {
            // SAFETY: restores the attributes read in `enter` on the same fd.
            unsafe {
                libc::tcsetattr(0, libc::TCSADRAIN, &raw const self.0);
            }
        }
    }

    /// Input that arrives within `timeout`, or `None`. End of input reads as an
    /// empty chunk.
    fn read_within(timeout: Duration) -> Option<Vec<u8>> {
        let mut pfd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&raw mut pfd, 1, ms) };
        if ready <= 0 {
            return None;
        }
        let mut buf = [0u8; 64];
        // SAFETY: reads into a live 64-byte buffer.
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        Some(buf[..usize::try_from(n).unwrap_or(0)].to_vec())
    }

    /// Hide the caret, redraw, show it, all inside one synchronized-output bracket.
    fn bracket(body: &str) {
        w(&format!("\x1b[?2026h\x1b[?25l{body}\x1b[?25h\x1b[?2026l"));
    }

    fn claude() {
        const SPIN: [char; 4] = ['|', '/', '-', '\\'];
        const INPUT_ROW: usize = 12;
        const SPIN_ROW: usize = 10;
        const PROMPT: &str = "> ";
        let caret = |buf: &str| {
            format!(
                "\x1b[{INPUT_ROW};{}H",
                PROMPT.len() + buf.chars().count() + 1
            )
        };
        let draw_input = |buf: &str| {
            bracket(&format!(
                "\x1b[{INPUT_ROW};1H\x1b[2K{PROMPT}{buf}{}",
                caret(buf)
            ))
        };
        let draw_spinner = |i: usize, buf: &str| {
            bracket(&format!(
                "\x1b[{SPIN_ROW};1H\x1b[2K  {} thinking... ({i}){}",
                SPIN[i % 4],
                caret(buf)
            ));
        };
        let raw = Raw::enter();
        w("\x1b[?1049h\x1b[2J");
        bracket("\x1b[1;1H\x1b[2K== fake claude ==\x1b[8;1H\x1b[2Kctx: streaming");
        let mut buf = String::new();
        draw_input(&buf);
        let mut i = 0;
        let mut last_spin = Instant::now();
        loop {
            if let Some(chunk) = read_within(Duration::from_millis(20)) {
                let text = String::from_utf8_lossy(&chunk);
                if text.contains(['\x03', 'Q']) {
                    break;
                }
                for c in text.chars() {
                    if c == '\x7f' {
                        buf.pop();
                    } else if !c.is_control() {
                        buf.push(c);
                    }
                }
                draw_input(&buf);
            }
            if last_spin.elapsed() >= Duration::from_millis(150) {
                last_spin = Instant::now();
                i += 1;
                draw_spinner(i, &buf);
            }
        }
        drop(raw);
        w("\x1b[?25h\x1b[?2026l\x1b[?1049l");
        println!("fake claude done");
    }

    fn streamer() {
        w("\x1b[?1049h\x1b[2J\x1b[H");
        w("streamer-shape: type below; streamer runs above\r\n\r\n> ");
        // One writer at a time, so a token chunk and an echo never interleave.
        let out = Arc::new(Mutex::new(()));
        let stop = Arc::new(AtomicBool::new(false));
        let tokens = {
            let (out, stop) = (Arc::clone(&out), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut n = 0;
                while !stop.load(Ordering::SeqCst) {
                    n += 1;
                    {
                        let _turn = out
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        w(&format!(
                            "\x1b7\x1b[1;1Hstreaming tokens {n} {}\x1b8",
                            ".".repeat(n % 20)
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
            })
        };
        let raw = Raw::enter();
        loop {
            let Some(chunk) = read_within(Duration::from_secs(3600)) else {
                continue;
            };
            if chunk.is_empty() || chunk.iter().any(|b| matches!(b, 0x03 | 0x04 | b'q')) {
                break;
            }
            let _turn = out
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            w(&String::from_utf8_lossy(&chunk));
        }
        stop.store(true, Ordering::SeqCst);
        let _ = tokens.join();
        drop(raw);
        w("\x1b[?1049l");
    }

    fn cold_streamer() {
        const TOKENS: &[u8] = b"qxv7 zt4m ph9k rw2n bd6f jg3s lc8v mx5q vk1r nz0w tf2h dp5y ";
        // Raw only to silence the line discipline's own echo: nothing is typed.
        let raw = Raw::enter();
        // The caret colour is set before any output, so frame 0 carries it.
        w("\x1b]12;rgb:d0/d0/d0\x07\x1b[?1049h\x1b[2J\x1b[H\x1b[?25h");
        w("cold-streamer :: caret walks, nothing typed");
        // Start near the bottom, so the folds scroll the alt screen within seconds.
        w("\x1b[18;1H");
        let mut n = 0_usize;
        loop {
            if let Some(chunk) = read_within(Duration::from_millis(30)) {
                if chunk.is_empty() || chunk.iter().any(|b| matches!(b, 0x03 | 0x04 | b'q')) {
                    break;
                }
                continue;
            }
            if n % 12 == 11 {
                w(&format!(" |{:03}| ", n / 12)); // the multi-cell arm
            } else {
                w(&char::from(TOKENS[n % TOKENS.len()]).to_string()); // one cell
            }
            if n % 24 == 23 {
                w("\r\n");
            }
            n += 1;
        }
        drop(raw);
        w("\x1b[?25h\x1b[?1049l");
    }

    fn repainter() {
        // A synchronized-output bracket around a bare cursor position: no cell is
        // written, erased or scrolled, so no content mark lands.
        const SPLIT: &str = "\x1b[?2026h\x1b[3;20H\x1b[?2026l";
        const WARM_LINES: usize = 10;
        let quit = |timeout: Duration| {
            read_within(timeout).is_some_and(|chunk| chunk.iter().any(|b| matches!(b, 0x03 | b'Q')))
        };
        let raw = Raw::enter();
        w("\x1b[?1049h");
        let mut done = false;
        for i in 0..WARM_LINES {
            w(&format!("\x1b[{};1Hwarming {i}\r\n", 1 + i % WARM_LINES));
            if quit(Duration::from_millis(100)) {
                done = true;
                break;
            }
        }
        while !done {
            w(SPLIT);
            done = quit(Duration::from_micros(166_667));
        }
        drop(raw);
        w("\x1b[?25h\x1b[?2026l\x1b[?1049l");
        println!("steady repainter done");
    }

    /// Run the shape `args` name; `false` for anything else.
    pub fn run(args: &[&str]) -> bool {
        match args {
            ["--shape", "claude"] => claude(),
            ["--shape", "streamer"] => streamer(),
            ["--shape", "cold-streamer"] => cold_streamer(),
            ["--shape", "repainter"] => repainter(),
            _ => return false,
        }
        true
    }
}
