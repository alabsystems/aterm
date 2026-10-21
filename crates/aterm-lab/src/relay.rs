// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The PTY relay of `nudge-trial`: a transparent go-between for the REAL
//! Claude Code and aterm's PTY.
//!
//! ```text
//! aterm's PTY  <-->  this relay  <-->  an inner PTY  <-->  the program (Claude Code)
//! ```
//!
//! WHAT IT IS FOR. aterm repairs a tab a resize left drawn one row off (the
//! repaint nudge) by telling that one program its terminal is a row shorter
//! and then its true size again. To test that against the real program a tab
//! has to be displaced on demand, and a displacement needs a resize the
//! program never hears about. Every size change of aterm's PTY reaches the
//! relay (SIGWINCH), which passes it on to the inner PTY unless it is HELD. A
//! flap written to the control socket while the relay is held (shrink, some
//! output, grow) moves aterm's grid and leaves Claude Code with the screen it
//! painted: the incident, forced.
//!
//! * `SIGUSR1` HOLD: size changes of the outer PTY are no longer passed on.
//! * `SIGUSR2` RELEASE: the outer size AS IT IS NOW is passed on (nothing
//!   happens when it equals the inner one).
//!
//! Nothing else is touched: bytes go both ways as they come, and the relay
//! signals nothing but its own child (at its exit). The nudge's own resize
//! pair passes through a released relay like any other resize, which is the
//! point: it must reach the program for the repair to work.
//!
//! THE NAME. The relay is this program copied under the name `node`, and the
//! name is load-bearing: aterm names a tab's program by its executable (the
//! foreground process of the tab's PTY, here the relay), and for `node`, `bun`
//! and `deno`, the runtimes an npm install of Claude Code runs under, it reads
//! the SCREEN to tell whether an agent is behind them. So the relay is classed
//! by what Claude Code draws, as a Claude Code started by npm is, with no dev
//! seam; under any other name it would be an unknown program and the nudge
//! would not run at all.
//!
//! `node [--log FILE] -- command [args…]`: FILE gets one line per event
//! (`HOLD`, `RELEASE`, `held RxC` for a size change not passed on, `told RxC`
//! for new rows or columns passed on, `pixels RxC` for new pixels only), so a
//! trial can say what the program was told. The inner program gets a session
//! and a controlling terminal of its own (`ctty`, below), so the kernel tells
//! it of every size the relay passes on.
//!
//! THE RECORDER. `aterm-lab ptyrec <log> -- command [args…]` is the same relay
//! run by hand in a terminal, keeping a TAKE of every chunk: `<t_ns> O <hex>`
//! for the program's output and `<t_ns> I <hex>` for the input passed to it,
//! `t_ns` the monotonic clock's nanoseconds, one line per read, appended. The
//! program's window follows the outer terminal's at the start and at every
//! SIGWINCH (24x80 when the outer side is no terminal). The real-byte replays
//! of aterm-effects (`tests/common/mod.rs`, `recording`) read these takes; the
//! line shape is theirs.

use crate::sys::{self, WinSize};
use std::io::Write as _;

fn note(log: &mut Option<std::fs::File>, what: &str, ws: Option<&WinSize>) {
    let Some(f) = log else { return };
    let ms = crate::util::now_ms();
    let size = ws
        .map(|w| format!(" {}x{}", w.ws_row, w.ws_col))
        .unwrap_or_default();
    let _ = f.write_all(
        format!(
            "{}.{:03} {what}{size}\n",
            ms.div_euclid(1000),
            ms.rem_euclid(1000)
        )
        .as_bytes(),
    );
}

fn same(a: &WinSize, b: &WinSize) -> bool {
    (a.ws_row, a.ws_col, a.ws_xpixel, a.ws_ypixel) == (b.ws_row, b.ws_col, b.ws_xpixel, b.ws_ypixel)
}

/// Pass the outer size on to the inner PTY when it differs (the kernel then
/// signals the program). aterm sets pixels with every resize, so a release can
/// pass on new pixels with the same rows and columns: that is `pixels`; a new
/// count of rows or columns is `told`.
fn pass_size_on(master: i32, inner: &mut WinSize, log: &mut Option<std::fs::File>) {
    let Some(outer) = sys::winsize(0) else { return };
    if same(&outer, inner) {
        return;
    }
    sys::set_winsize(master, &outer);
    let what = if (outer.ws_row, outer.ws_col) == (inner.ws_row, inner.ws_col) {
        "pixels"
    } else {
        "told"
    };
    note(log, what, Some(&outer));
    *inner = outer;
}

/// A take of `ptyrec` (module doc): the open log, written a line per chunk.
struct Take(std::fs::File);

/// Which way a chunk went: `O` the program wrote it, `I` it was passed to the program.
#[derive(Clone, Copy)]
pub(crate) enum Way {
    Out,
    In,
}

impl Take {
    /// One chunk's line, `<t_ns> O|I <hex>`, written whole. A take that can
    /// no longer be written is reported once and closed; the relay goes on,
    /// the program never told.
    fn chunk(take: &mut Option<Self>, way: Way, bytes: &[u8]) {
        let Some(Take(f)) = take else { return };
        let line = take_line(sys::monotonic_ns(), way, bytes);
        if let Err(e) = f.write_all(line.as_bytes()) {
            eprintln!("\r\nptyrec: the take stops here: {e}\r");
            *take = None;
        }
    }
}

/// A take's line (module doc): the stamp, the way, the bytes in lower-case hex.
pub(crate) fn take_line(t_ns: u64, way: Way, bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut line = String::with_capacity(bytes.len() * 2 + 24);
    let _ = write!(
        line,
        "{t_ns} {} ",
        match way {
            Way::Out => 'O',
            Way::In => 'I',
        }
    );
    for b in bytes {
        let _ = write!(line, "{b:02x}");
    }
    line.push('\n');
    line
}

/// Copy what one read gives from `from` to `to`, into the take as `way`
/// first; `false` when `from` is finished or `to` is gone.
fn pump(from: i32, to: i32, take: &mut Option<Take>, way: Way) -> bool {
    let mut buf = [0u8; 65536];
    match sys::read(from, &mut buf) {
        Ok(0) => false,
        Ok(n) => {
            Take::chunk(take, way, &buf[..n]);
            sys::write_all(to, &buf[..n]).is_ok()
        }
        Err(e) => matches!(
            e.kind(),
            std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
        ),
    }
}

/// The program's last output (its goodbye: leaving the alternate screen) is still queued when it exits.
fn drain(master: i32, take: &mut Option<Take>) {
    loop {
        let mut fds = [sys::pollfd(master)];
        if sys::poll(&mut fds, 0).unwrap_or(0) == 0 || !pump(master, 1, take, Way::Out) {
            return;
        }
    }
}

/// `[--log FILE] -- command [args…]` → (the log, the command); `None` when the
/// grammar is not met (no `--`, or nothing after it).
#[must_use]
pub fn parse_args(args: &[String]) -> Option<(Option<String>, &[String])> {
    let (logpath, rest) = match args {
        [flag, path, rest @ ..] if flag == "--log" => (Some(path.clone()), rest),
        _ => (None, args),
    };
    match rest.split_first() {
        Some((dashes, command)) if dashes == "--" && !command.is_empty() => {
            Some((logpath, command))
        }
        _ => None,
    }
}

/// `node [--log FILE] -- command [args…]`.
#[must_use]
pub fn main(args: &[String]) -> i32 {
    let Some((logpath, command)) = parse_args(args) else {
        eprintln!("usage: node [--log FILE] -- command [args...]");
        return 2;
    };
    let log = logpath.and_then(|p| append(&p).ok());
    relay(command, log, None)
}

/// `aterm-lab ptyrec <log> -- command [args…]` (module doc): the relay, by
/// hand, keeping a take in `<log>` (appended). Exit: the program's.
#[must_use]
pub fn ptyrec(args: &[String]) -> i32 {
    let Some((take, command)) = parse_ptyrec(args) else {
        eprintln!("usage: aterm-lab ptyrec <log> -- command [args...]");
        return 2;
    };
    match append(take) {
        Ok(f) => relay(command, None, Some(Take(f))),
        Err(e) => {
            eprintln!("ptyrec: {take}: {e}");
            1
        }
    }
}

/// `<log> -- command [args…]` → (the take's path, the command).
#[must_use]
pub fn parse_ptyrec(args: &[String]) -> Option<(&str, &[String])> {
    match args {
        [take, dashes, command @ ..] if dashes == "--" && !command.is_empty() => {
            Some((take.as_str(), command))
        }
        _ => None,
    }
}

fn append(path: &str) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// The size a take hands on when its outer side is no terminal: ptyrec's
/// 24x80. The nudge relay's outer side is a tab, and with none it hands on
/// 0x0, as it always has ([`relay`]).
const NO_TERMINAL: WinSize = WinSize {
    ws_row: 24,
    ws_col: 80,
    ws_xpixel: 0,
    ws_ypixel: 0,
};

/// No size: what the nudge relay hands on when its outer side is no terminal.
const NO_SIZE: WinSize = WinSize {
    ws_row: 0,
    ws_col: 0,
    ws_xpixel: 0,
    ws_ypixel: 0,
};

/// The relay proper: `command` behind an inner PTY, bytes both ways, the
/// outer size passed on unless held; `log` gets its events, `take` every chunk.
fn relay(command: &[String], mut log: Option<std::fs::File>, mut take: Option<Take>) -> i32 {
    // Handlers first: a signal between the spawn and the loop is queued, not fatal (SIGUSR1's default).
    let Ok(signals) = sys::SignalPipe::install(&[
        libc::SIGWINCH,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGCHLD,
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGINT,
    ]) else {
        return 1;
    };
    // Raw on the outer side, so bytes go through untouched (put back when this ends).
    let raw = sys::RawMode::enter(0);
    let mut inner = sys::winsize(0).unwrap_or(if take.is_some() { NO_TERMINAL } else { NO_SIZE });
    let (master, slave) = match aterm_pty::open_pty_pair() {
        Ok(pair) => pair,
        Err(e) => {
            drop(raw);
            eprintln!("relay: openpty: {e}");
            return 1;
        }
    };
    sys::set_winsize(master, &inner);
    let slave = sys::own_fd(slave);
    let spawned = (|| {
        let (a, b, c) = (slave.try_clone()?, slave.try_clone()?, slave.try_clone()?);
        let exe = std::env::current_exe()?;
        // named for the CLI, not `node`: under that name this program is the relay again
        aterm_uds::spawnfd::command(exe)
            .arg0("aterm-lab")
            .arg("ctty")
            .arg("--")
            .args(command)
            .setsid(true)
            .stdin(a)
            .stdout(b)
            .stderr(c)
            .spawn()
    })();
    drop(slave);
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            drop(raw);
            eprintln!("relay: spawn: {e}");
            return 1;
        }
    };
    let mut held = false;
    let mut status = None;
    'run: loop {
        let mut fds = [
            sys::pollfd(0),
            sys::pollfd(master),
            sys::pollfd(signals.read_fd),
        ];
        if sys::poll(&mut fds, -1).is_err() {
            break;
        }
        if fds[2].revents != 0 {
            for sig in signals.drain() {
                match sig {
                    libc::SIGUSR1 => {
                        held = true;
                        note(&mut log, "HOLD", None);
                    }
                    libc::SIGUSR2 => {
                        held = false;
                        note(&mut log, "RELEASE", None);
                        pass_size_on(master, &mut inner, &mut log);
                    }
                    libc::SIGWINCH => {
                        if !held {
                            pass_size_on(master, &mut inner, &mut log);
                        } else if let Some(outer) = sys::winsize(0) {
                            note(&mut log, "held", Some(&outer));
                        }
                    }
                    libc::SIGCHLD => {
                        if let Ok(Some(st)) = child.try_wait() {
                            status = Some(st);
                            drain(master, &mut take);
                            break 'run;
                        }
                    }
                    // TERM, HUP, INT: leave cleanly
                    _ => break 'run,
                }
            }
        }
        let ready = |i: usize| fds[i].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
        if ready(1) && !pump(master, 1, &mut take, Way::Out) {
            break;
        }
        if ready(0) && !pump(0, master, &mut take, Way::In) {
            break;
        }
    }
    drop(raw);
    // The program's terminal hangs up (SIGHUP); one that ignores it is killed. Its own child only is signalled.
    sys::close(master);
    let status = status.or_else(|| {
        for _ in 0..40 {
            if let Ok(Some(st)) = child.try_wait() {
                return Some(st);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = child.kill();
        child.wait().ok()
    });
    use std::os::unix::process::ExitStatusExt as _;
    status.map_or(1, |s| {
        s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
    })
}

/// `aterm-lab ctty [--arg0 NAME] -- command…`: take stdin's terminal as this
/// session's controlling terminal, then become `command` (its `argv[0]` NAME
/// when given: the link relay starts a login shell as `-zsh`,
/// [`crate::link`]). The relay starts its program this way, in a session of
/// its own, so the kernel tells the program of every size change (SIGWINCH
/// goes to a terminal's foreground group) without code between a fork and an
/// exec.
#[must_use]
pub fn ctty(args: &[String]) -> i32 {
    let (arg0, args) = match args {
        [flag, name, rest @ ..] if flag == "--arg0" => (Some(name.as_str()), rest),
        _ => (None, args),
    };
    let Some((None, command)) = parse_args(args) else {
        eprintln!("usage: aterm-lab ctty [--arg0 NAME] -- command [args...]");
        return 2;
    };
    if let Err(e) = sys::take_controlling_tty(0) {
        eprintln!("relay: TIOCSCTTY: {e}");
        return 1;
    }
    let mut exec = aterm_uds::spawnfd::command(&command[0]);
    if let Some(name) = arg0 {
        exec.arg0(name);
    }
    let err = exec.args(&command[1..]).exec();
    eprintln!("relay: exec {}: {err}", command[0]);
    127
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_relay_takes_an_optional_log_then_dashes_then_a_command() {
        let a = v(&["--log", "/r/relay.log", "--", "env", "A=1", "claude"]);
        let (log, cmd) = parse_args(&a).unwrap();
        assert_eq!(log.as_deref(), Some("/r/relay.log"));
        assert_eq!(cmd, &v(&["env", "A=1", "claude"])[..]);
        let a = v(&["--", "true"]);
        assert_eq!(parse_args(&a).map(|(l, c)| (l, c.len())), Some((None, 1)));
        // the ctty helper's own argument list is not a relay's
        assert!(parse_args(&v(&["ctty", "--", "true"])).is_none());
        assert!(parse_args(&v(&["--log", "x", "--"])).is_none());
        assert!(parse_args(&v(&["echo", "hi"])).is_none());
    }

    /// The recorder's grammar, and its line: the shape aterm-effects'
    /// `recording` parses (`<stamp> <O|I> <hex>`, lower-case, two digits a byte).
    #[test]
    fn a_take_names_its_log_then_dashes_then_a_command_and_writes_ptyrecs_lines() {
        let a = v(&["/r/take.log", "--", "claude", "--resume"]);
        let (take, cmd) = parse_ptyrec(&a).unwrap();
        assert_eq!(take, "/r/take.log");
        assert_eq!(cmd, &v(&["claude", "--resume"])[..]);
        assert!(parse_ptyrec(&v(&["/r/take.log", "--"])).is_none());
        assert!(parse_ptyrec(&v(&["/r/take.log", "claude"])).is_none());
        assert!(parse_ptyrec(&v(&["--", "claude"])).is_none());
        assert_eq!(
            take_line(17, Way::Out, b"\x1b[0mA\n"),
            "17 O 1b5b306d410a\n"
        );
        assert_eq!(take_line(18, Way::In, b"\r"), "18 I 0d\n");
        assert_eq!(take_line(19, Way::In, b""), "19 I \n");
    }
}
