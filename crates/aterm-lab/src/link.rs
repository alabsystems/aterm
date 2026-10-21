// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LINK WITH A ROUND TRIP: a PTY relay that holds every chunk for half a
//! round trip each way, so a tab's shell answers a key as a shell over a slow
//! link does (`predict-live`, [`crate::predict_live`]).
//!
//! ```text
//! aterm's PTY  <-- rtt/2 -->  this relay  <-- rtt/2 -->  an inner PTY  <-->  the shell
//! ```
//!
//! WHY. aterm's predictor shows a key's effect before its echo only on a link
//! it has MEASURED as slow (`Adaptive` opens at 20 ms), from the press to the
//! echo it credits. A local shell answers in a millisecond, so nothing is ever
//! predicted on glass there; ssh to a far host is a link nobody controls. This
//! relay is the honest middle: the shell is a real zsh or bash, the bytes are
//! the bytes it writes, and the only change is time. Each chunk read from one
//! side is written to the other `rtt/2` later, in the order read, whole: a link
//! with a fixed latency, no jitter and no loss. Its outer side is raw (no echo,
//! no line discipline), as ssh's is, so the program behind it echoes for itself.
//!
//! THE NAME. aterm integrates a tab's shell by the program it spawns
//! (`ShellType::detect` reads the file name): the relay is this program copied
//! under the shell's own name (`zsh`, `bash`), and aterm's `shell` setting
//! names the copy, so the shell behind it gets aterm's integration (zsh's
//! `ZDOTDIR`, bash's `--rcfile` argument, the nonce that signs its prompt
//! marks) exactly as a shell aterm spawned itself does. The relay passes the
//! arguments and the environment through, starts `/bin/<name>` (or
//! `LAB_LINK_EXEC`) with the `argv[0]` it was given (`-zsh`, a login shell),
//! and takes its own settings from the environment, removed before the shell
//! starts:
//!
//! * `LAB_LINK_RTT_MS`: the round trip, in milliseconds (default 100);
//! * `LAB_LINK_TAKE`: a directory; each relay appends a TAKE of the far end's
//!   own view to `<dir>/<pid>.take`, ptyrec's line (`<t_ns> I <hex>` when the
//!   shell is handed input, `<t_ns> O <hex>` when it writes, and `<t_ns> W
//!   <rows>x<cols>` for the window it is told, at the start and at every
//!   resize), so a reader can tell which output a key caused (what the shell
//!   wrote after it read the key and before it read the next) and replay the
//!   far end's own screen;
//! * `LAB_LINK_EXEC`: the shell to start, when not `/bin/<name>`.
//!
//! `aterm-lab link [--rtt MS] [--take FILE] -- command [args…]` is the same
//! relay run by hand.

use crate::sys::{self, WinSize};
use std::collections::VecDeque;
use std::io::Write as _;
use std::time::{Duration, Instant};

/// The settings a relay takes from its environment ([`Settings::from_env`]).
pub const ENV_RTT: &str = "LAB_LINK_RTT_MS";
pub const ENV_TAKE: &str = "LAB_LINK_TAKE";
pub const ENV_EXEC: &str = "LAB_LINK_EXEC";

/// The round trip when none is named.
const DEFAULT_RTT_MS: u64 = 100;

/// What one relay does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The round trip: each way holds a chunk half of it.
    pub rtt: Duration,
    /// The take file, when one is kept.
    pub take: Option<String>,
}

impl Settings {
    /// Half the round trip: how long each chunk is held, each way.
    #[must_use]
    pub fn one_way(&self) -> Duration {
        self.rtt / 2
    }

    /// The settings of a relay started as a shell, from `get` (the
    /// environment): the round trip, and a take in `<dir>/<pid>.take`.
    ///
    /// # Errors
    /// `LAB_LINK_RTT_MS` is not a whole number of milliseconds.
    pub fn from_env(get: impl Fn(&str) -> Option<String>, pid: u32) -> Result<Settings, String> {
        let rtt = match get(ENV_RTT) {
            None => DEFAULT_RTT_MS,
            Some(v) => v
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("{ENV_RTT}={v:?} is not a whole number of milliseconds"))?,
        };
        Ok(Settings {
            rtt: Duration::from_millis(rtt),
            take: get(ENV_TAKE)
                .filter(|d| !d.is_empty())
                .map(|d| format!("{}/{pid}.take", d.trim_end_matches('/'))),
        })
    }
}

/// The shell a relay named `name` (`-zsh`, `bash`) starts: `LAB_LINK_EXEC`
/// when set, else `/bin/<name>` without a login dash.
#[must_use]
pub fn shell_for(name: &str, exec: Option<String>) -> String {
    exec.filter(|e| !e.is_empty())
        .unwrap_or_else(|| format!("/bin/{}", name.trim_start_matches('-')))
}

/// `[--rtt MS] [--take FILE] -- command [args…]` → (the settings, the
/// command); `None` when the grammar is not met.
#[must_use]
pub fn parse_args(args: &[String]) -> Option<(Settings, &[String])> {
    let mut settings = Settings {
        rtt: Duration::from_millis(DEFAULT_RTT_MS),
        take: None,
    };
    let mut rest = args;
    loop {
        match rest {
            [flag, ms, tail @ ..] if flag == "--rtt" => {
                settings.rtt = Duration::from_millis(ms.parse().ok()?);
                rest = tail;
            }
            [flag, file, tail @ ..] if flag == "--take" => {
                settings.take = Some(file.clone());
                rest = tail;
            }
            [dashes, command @ ..] if dashes == "--" && !command.is_empty() => {
                return Some((settings, command));
            }
            _ => return None,
        }
    }
}

/// One way of the link: chunks in the order read, each due `one_way` after it
/// was read. Time is handed in, so a test steps it.
#[derive(Debug, Default)]
pub struct Way {
    one_way: Duration,
    queue: VecDeque<(Instant, Vec<u8>)>,
}

impl Way {
    #[must_use]
    pub fn new(one_way: Duration) -> Way {
        Way {
            one_way,
            queue: VecDeque::new(),
        }
    }

    /// A chunk read at `now`.
    pub fn push(&mut self, now: Instant, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.queue.push_back((now + self.one_way, bytes.to_vec()));
        }
    }

    /// When the oldest chunk is due, if any is held.
    #[must_use]
    pub fn next_due(&self) -> Option<Instant> {
        self.queue.front().map(|(due, _)| *due)
    }

    /// The oldest chunk, once it is due at `now`.
    pub fn pop_due(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.next_due().is_some_and(|due| due <= now) {
            self.queue.pop_front().map(|(_, bytes)| bytes)
        } else {
            None
        }
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// The `poll` timeout until the earlier of `a` and `b`, in whole
/// milliseconds rounded up (a chunk is never written early), `0` when one is
/// already due, `-1` (wait for a descriptor) when neither is held.
#[must_use]
pub fn wait_ms(now: Instant, a: Option<Instant>, b: Option<Instant>) -> i32 {
    let Some(due) = [a, b].into_iter().flatten().min() else {
        return -1;
    };
    let left = due.saturating_duration_since(now);
    let ms = left.as_nanos().div_ceil(1_000_000);
    i32::try_from(ms).unwrap_or(i32::MAX)
}

/// The far end's take: a line per chunk, ptyrec's shape.
struct Take(Option<std::fs::File>);

impl Take {
    fn open(path: Option<&str>) -> Take {
        Take(path.and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .map_err(|e| eprintln!("\r\nlink: the take {p}: {e}\r"))
                .ok()
        }))
    }

    /// The far end's window as it is told it: `<t_ns> W <rows>x<cols>`, at the
    /// start and at every size passed on, so a replay of the take draws at the
    /// size the far end drew at.
    fn size(&mut self, ws: &WinSize) {
        let Some(f) = &mut self.0 else { return };
        let line = format!("{} W {}x{}\n", sys::monotonic_ns(), ws.ws_row, ws.ws_col);
        if f.write_all(line.as_bytes()).is_err() {
            self.0 = None;
        }
    }

    fn chunk(&mut self, way: crate::relay::Way, bytes: &[u8]) {
        let Some(f) = &mut self.0 else { return };
        let line = crate::relay::take_line(sys::monotonic_ns(), way, bytes);
        if f.write_all(line.as_bytes()).is_err() {
            self.0 = None;
        }
    }
}

/// The relay started under a shell's name (`zsh`, `-zsh`, `bash`, `-bash`):
/// `argv[0]` is `name`, `args` the shell's own arguments.
#[must_use]
pub fn as_shell(name: &str, args: &[String]) -> i32 {
    let get = |k: &str| std::env::var(k).ok();
    let settings = match Settings::from_env(get, std::process::id()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("link: {e}");
            return 2;
        }
    };
    let shell = shell_for(name, get(ENV_EXEC));
    let mut command = vec![shell];
    command.extend(args.iter().cloned());
    relay(&settings, Some(name), &command)
}

/// `aterm-lab link [--rtt MS] [--take FILE] -- command [args…]`.
#[must_use]
pub fn main(args: &[String]) -> i32 {
    let Some((settings, command)) = parse_args(args) else {
        eprintln!("usage: aterm-lab link [--rtt MS] [--take FILE] -- command [args...]");
        return 2;
    };
    relay(&settings, None, command)
}

/// No size: what the relay hands on when its outer side is no terminal.
const NO_SIZE: WinSize = WinSize {
    ws_row: 24,
    ws_col: 80,
    ws_xpixel: 0,
    ws_ypixel: 0,
};

/// The relay proper: `command` behind an inner PTY (started with `arg0` when
/// given), every chunk held `rtt/2` each way.
fn relay(settings: &Settings, arg0: Option<&str>, command: &[String]) -> i32 {
    let Ok(signals) = sys::SignalPipe::install(&[
        libc::SIGWINCH,
        libc::SIGCHLD,
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGINT,
    ]) else {
        return 1;
    };
    let raw = sys::RawMode::enter(0);
    let size = sys::winsize(0).unwrap_or(NO_SIZE);
    let (master, slave) = match aterm_pty::open_pty_pair() {
        Ok(pair) => pair,
        Err(e) => {
            drop(raw);
            eprintln!("link: openpty: {e}");
            return 1;
        }
    };
    sys::set_winsize(master, &size);
    let slave = sys::own_fd(slave);
    let spawned = (|| {
        let (a, b, c) = (slave.try_clone()?, slave.try_clone()?, slave.try_clone()?);
        let exe = std::env::current_exe()?;
        let mut cmd = aterm_uds::spawnfd::command(exe);
        // named for the CLI: under a shell's name this program is the relay again
        cmd.arg0("aterm-lab").arg("ctty");
        if let Some(name) = arg0 {
            cmd.arg("--arg0").arg(name);
        }
        cmd.arg("--")
            .args(command)
            .env_remove(ENV_RTT)
            .env_remove(ENV_TAKE)
            .env_remove(ENV_EXEC)
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
            eprintln!("link: spawn: {e}");
            return 1;
        }
    };
    let mut take = Take::open(settings.take.as_deref());
    take.size(&size);
    let mut inward = Way::new(settings.one_way());
    let mut outward = Way::new(settings.one_way());
    let mut status = None;
    let mut input_open = true;
    let mut output_open = true;
    let mut buf = vec![0u8; 65536];
    loop {
        let now = Instant::now();
        while let Some(bytes) = inward.pop_due(now) {
            take.chunk(crate::relay::Way::In, &bytes);
            if sys::write_all(master, &bytes).is_err() {
                output_open = false;
            }
        }
        while let Some(bytes) = outward.pop_due(now) {
            if sys::write_all(1, &bytes).is_err() {
                input_open = false;
            }
        }
        // The program is gone and what it wrote has crossed: done.
        if !output_open && outward.is_empty() {
            break;
        }
        if !input_open && outward.is_empty() {
            break;
        }
        let timeout = wait_ms(Instant::now(), inward.next_due(), outward.next_due());
        let mut fds = [
            sys::pollfd(if input_open { 0 } else { -1 }),
            sys::pollfd(if output_open { master } else { -1 }),
            sys::pollfd(signals.read_fd),
        ];
        if sys::poll(&mut fds, timeout).is_err() {
            break;
        }
        if fds[2].revents != 0 {
            for sig in signals.drain() {
                match sig {
                    libc::SIGWINCH => {
                        if let Some(outer) = sys::winsize(0) {
                            sys::set_winsize(master, &outer);
                            take.size(&outer);
                        }
                    }
                    libc::SIGCHLD => {
                        if let Ok(Some(st)) = child.try_wait() {
                            status = Some(st);
                            // What it wrote last is still queued: read it all,
                            // held like the rest, then the master is done.
                            loop {
                                let mut fds = [sys::pollfd(master)];
                                if sys::poll(&mut fds, 0).unwrap_or(0) == 0 {
                                    break;
                                }
                                match sys::read(master, &mut buf) {
                                    Ok(n) if n > 0 => {
                                        take.chunk(crate::relay::Way::Out, &buf[..n]);
                                        outward.push(Instant::now(), &buf[..n]);
                                    }
                                    _ => break,
                                }
                            }
                            output_open = false;
                        }
                    }
                    // TERM, HUP, INT: leave now
                    _ => {
                        output_open = false;
                        input_open = false;
                    }
                }
            }
        }
        let ready = |i: usize| fds[i].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
        if output_open && ready(1) {
            match sys::read(master, &mut buf) {
                Ok(0) => output_open = false,
                Ok(n) => {
                    take.chunk(crate::relay::Way::Out, &buf[..n]);
                    outward.push(Instant::now(), &buf[..n]);
                }
                Err(e) if is_transient(&e) => {}
                Err(_) => output_open = false,
            }
        }
        if input_open && ready(0) {
            match sys::read(0, &mut buf) {
                Ok(0) => input_open = false,
                Ok(n) => inward.push(Instant::now(), &buf[..n]),
                Err(e) if is_transient(&e) => {}
                Err(_) => input_open = false,
            }
        }
    }
    drop(raw);
    sys::close(master);
    let status = status.or_else(|| {
        for _ in 0..40 {
            if let Ok(Some(st)) = child.try_wait() {
                return Some(st);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        child.wait().ok()
    });
    use std::os::unix::process::ExitStatusExt as _;
    status.map_or(1, |s| {
        s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
    })
}

fn is_transient(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    /// Each way holds every chunk for half the round trip, in the order read,
    /// whole; nothing is handed on before it is due.
    #[test]
    fn a_way_hands_each_chunk_on_half_a_round_trip_after_it_was_read_in_order() {
        let t0 = Instant::now();
        let half = Duration::from_millis(50);
        let mut way = Way::new(half);
        assert_eq!(way.next_due(), None);
        way.push(t0, b"a");
        way.push(t0 + Duration::from_millis(10), b"\x1b[D");
        way.push(t0 + Duration::from_millis(10), b"");
        assert_eq!(way.next_due(), Some(t0 + half));
        assert_eq!(way.pop_due(t0 + Duration::from_millis(49)), None);
        assert_eq!(way.pop_due(t0 + half).as_deref(), Some(&b"a"[..]));
        assert_eq!(way.pop_due(t0 + half), None, "the second is not due yet");
        assert_eq!(way.next_due(), Some(t0 + Duration::from_millis(60)));
        assert_eq!(
            way.pop_due(t0 + Duration::from_millis(500)).as_deref(),
            Some(&b"\x1b[D"[..])
        );
        assert!(way.is_empty(), "an empty read holds nothing");
    }

    /// The poll waits until the earlier chunk is due, rounded up, so it never
    /// wakes before one; nothing held waits on the descriptors alone.
    #[test]
    fn the_wait_is_until_the_earlier_due_chunk_rounded_up() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        assert_eq!(wait_ms(t0, None, None), -1);
        assert_eq!(wait_ms(t0, Some(t0 + ms(30)), Some(t0 + ms(7))), 7);
        assert_eq!(wait_ms(t0, Some(t0 + Duration::from_micros(1)), None), 1);
        assert_eq!(wait_ms(t0 + ms(9), Some(t0 + ms(7)), None), 0);
    }

    /// A relay run by hand names its round trip and its take, then the command.
    #[test]
    fn the_relay_by_hand_takes_a_round_trip_and_a_take_then_dashes_then_a_command() {
        let a = v(&["--rtt", "250", "--take", "/t/x.take", "--", "zsh", "-f"]);
        let (s, cmd) = parse_args(&a).unwrap();
        assert_eq!(s.rtt, Duration::from_millis(250));
        assert_eq!(s.one_way(), Duration::from_millis(125));
        assert_eq!(s.take.as_deref(), Some("/t/x.take"));
        assert_eq!(cmd, &v(&["zsh", "-f"])[..]);
        let (s, _) = parse_args(&v(&["--", "true"])).unwrap();
        assert_eq!(s.rtt, Duration::from_millis(DEFAULT_RTT_MS));
        assert!(parse_args(&v(&["--rtt", "x", "--", "true"])).is_none());
        assert!(parse_args(&v(&["--rtt", "5"])).is_none());
        assert!(parse_args(&v(&["zsh"])).is_none());
    }

    /// Started under a shell's name it reads its round trip and its take's
    /// directory from the environment, and starts the shell of that name.
    #[test]
    fn under_a_shells_name_it_starts_that_shell_with_the_environments_link() {
        let env = |k: &str| match k {
            ENV_RTT => Some("80".to_string()),
            ENV_TAKE => Some("/w/takes/".to_string()),
            _ => None,
        };
        let s = Settings::from_env(env, 42).unwrap();
        assert_eq!(s.rtt, Duration::from_millis(80));
        assert_eq!(s.take.as_deref(), Some("/w/takes/42.take"));
        let bare = Settings::from_env(|_| None, 1).unwrap();
        assert_eq!(bare.rtt, Duration::from_millis(DEFAULT_RTT_MS));
        assert_eq!(bare.take, None);
        assert!(Settings::from_env(|k| (k == ENV_RTT).then(|| "fast".into()), 1).is_err());
        assert_eq!(shell_for("-zsh", None), "/bin/zsh");
        assert_eq!(shell_for("bash", None), "/bin/bash");
        assert_eq!(
            shell_for("bash", Some("/opt/bash".into())),
            "/opt/bash",
            "LAB_LINK_EXEC names another"
        );
    }
}
