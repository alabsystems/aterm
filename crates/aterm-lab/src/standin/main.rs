// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-lab-standin`: the stand-in vendor agent `lab harness-proof` lays into its scratch atpkg
//! store as two builds (`<prefix>/store/<program>/<build>/bin/<cmd>`), so a harness runs it as its
//! child exactly as it runs the vendor's CLI, and no vendor byte, account or server is involved
//! (`docs/DESIGN-harness-repos-2026-10-24.md` §3.7, §9 step 2). Dev-only: never packed or shipped.
//!
//! It plays the vendor its name says (`argv[0]`'s file name; [`Dialect`]); Muse Code is the one
//! dialect so far. As Muse Code it keeps a session the way the real one does, so both the harness
//! and aterm's built-in Muse lanes read it as Muse Code: a `.session.lock` naming its pid, held open
//! for its life, beside a `session.jsonl` whose records are Muse Code's (`session.started`, the
//! process's `runtime.session.route_facts`, `runtime.session.metadata`, a run's `started` and
//! `terminal` records, and `session.end` with `exit_reason: clean` on a clean end), under
//! `$XDG_DATA_HOME/muse/sessions/YYYY/MM/DD/<uuid>/` (else `$HOME/.local/share`); `resume <uuid>`
//! reopens that session. It draws Muse Code's frame (aterm-phase's Muse reader: the `Muse Code
//! <version>` banner, a rule, the `❯` composer, a rule and a footer two spaces in), whose footer
//! says what aterm's screen read judges a leg by: `standin · <version> · pid <pid> · session <uuid>
//! · start <n>`, `<n>` counting the session's starts.
//!
//! Input is read raw, only when `poll` says the terminal is readable. Enter submits the composer:
//! `/crash` kills the process with `SIGKILL` and no end record,
//! `/exit <n>` records a clean end and exits `<n>`, and any other text is one turn that ends at
//! once. `SIGTERM` is a clean end (143); `SIGHUP` ends it by `SIGHUP`; `SIGINT` is said on the
//! screen and nothing more. `--help` and `--version` print and exit 0 (a harness's build probe).
//! Every start, probe, signal and end is one line in `$ATERM_LAB_STANDIN_LOG`, when set.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The vendor a stand-in plays, by the name it was run as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// Muse Code (`muse`).
    Muse,
}

impl Dialect {
    fn of(argv0: &OsString) -> Option<Dialect> {
        match Path::new(argv0).file_name()?.to_str()? {
            "muse" => Some(Dialect::Muse),
            _ => None,
        }
    }

    /// The vendor's product name, as its banner spells it.
    const fn banner(self) -> &'static str {
        match self {
            Dialect::Muse => "Muse Code",
        }
    }

    /// The vendor's data directory under the data home.
    const fn data_dir(self) -> &'static str {
        match self {
            Dialect::Muse => "muse",
        }
    }
}

static TERM: AtomicBool = AtomicBool::new(false);
static HUP: AtomicBool = AtomicBool::new(false);
static INT: AtomicBool = AtomicBool::new(false);
static WINCH: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(sig: libc::c_int) {
    match sig {
        libc::SIGTERM => TERM.store(true, Ordering::SeqCst),
        libc::SIGHUP => HUP.store(true, Ordering::SeqCst),
        libc::SIGINT => INT.store(true, Ordering::SeqCst),
        libc::SIGWINCH => WINCH.store(true, Ordering::SeqCst),
        _ => {}
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(0))
}

/// A string as a JSON string literal's body.
fn json(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out
}

/// A word for the start log: printable ASCII but space and `%` kept, every other byte `%XX`.
fn word(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Append `line` to `$ATERM_LAB_STANDIN_LOG`, if set; a failed write is dropped.
fn log(line: &str) {
    let Some(path) = std::env::var_os("ATERM_LAB_STANDIN_LOG") else {
        return;
    };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{} {line}\n", now_us()).as_bytes());
    }
}

fn read_regular_text(path: &Path) -> Option<String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= 16 * 1024 * 1024)
        .then(|| String::from_utf8(bytes).ok())
        .flatten()
}

/// The version this build's store record names: `<store>/<program>/<build>.vendor` beside
/// `exe`'s `<build>/bin/<cmd>`, its `version = "…"` line.
fn store_version(exe: &Path) -> Option<String> {
    let build = exe.parent()?.parent()?;
    let record = build.with_extension("vendor");
    let text = read_regular_text(&record)?;
    text.lines().find_map(|l| {
        let v = l.trim().strip_prefix("version")?.trim_start();
        let v = v.strip_prefix('=')?.trim().trim_matches('"');
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// A fresh lowercase UUIDv4 from `/dev/urandom`.
fn new_uuid() -> String {
    let mut b = [0u8; 16];
    let read = File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    if read.is_err() {
        let t = now_us().to_le_bytes();
        let p = std::process::id().to_le_bytes();
        b[..8].copy_from_slice(&t);
        b[8..12].copy_from_slice(&p);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

/// Whether `s` is a hyphenated 8-4-4-4-12 lowercase-or-uppercase hex UUID.
fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The civil date (UTC) of `secs` since the epoch, `(year, month, day)`.
fn civil(secs: u64) -> (i64, u32, u32) {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// The vendor's sessions root under the data home (`$XDG_DATA_HOME`, else `$HOME/.local/share`).
fn sessions_root(dialect: Dialect) -> Option<PathBuf> {
    let data = match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(x) if Path::new(&x).is_absolute() => PathBuf::from(x),
        Some(_) => return None,
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
    };
    Some(data.join(dialect.data_dir()).join("sessions"))
}

/// The directory of `session` under `root` (`YYYY/MM/DD/<session>`): the one that exists, else
/// today's.
fn session_dir(root: &Path, session: &str) -> PathBuf {
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    };
    let found = dirs(root)
        .iter()
        .flat_map(|y| dirs(y))
        .flat_map(|m| dirs(&m))
        .map(|d| d.join(session))
        .find(|s| s.is_dir());
    found.unwrap_or_else(|| {
        let (y, m, d) = civil(now_us() / 1_000_000);
        root.join(format!("{y:04}"))
            .join(format!("{m:02}"))
            .join(format!("{d:02}"))
            .join(session)
    })
}

/// The terminal's columns and rows, else 80x24.
fn size() -> (usize, usize) {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one `winsize` into the valid, owned `ws`.
    let ok = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &raw mut ws) } == 0;
    if ok && ws.ws_col > 0 && ws.ws_row > 0 {
        (usize::from(ws.ws_col), usize::from(ws.ws_row))
    } else {
        (80, 24)
    }
}

/// The terminal's mode at start, put back on a clean end.
struct Mode(Option<libc::termios>);

impl Mode {
    /// Take the mode and set raw input as a TUI does: no line editing, no echo, signals from keys
    /// kept.
    fn raw() -> Mode {
        // SAFETY: an all-zero termios is a valid out-slot for tcgetattr, which fills it.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: tcgetattr on fd 0 into the valid `t`.
        if unsafe { libc::tcgetattr(0, &raw mut t) } != 0 {
            return Mode(None);
        }
        let saved = t;
        t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::IEXTEN);
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        // SAFETY: tcsetattr on fd 0 from the valid `t`.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw const t) };
        Mode(Some(saved))
    }

    fn restore(&self) {
        if let Some(t) = &self.0 {
            // SAFETY: tcsetattr on fd 0 from the mode tcgetattr gave.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, t) };
        }
    }
}

fn install_handlers() {
    for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGWINCH] {
        // SAFETY: an all-zero sigaction is a valid starting value; every field set below is plain data.
        let mut act: libc::sigaction = unsafe { std::mem::zeroed() };
        act.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        act.sa_flags = 0;
        // SAFETY: sigemptyset on the owned mask; sigaction with a valid action and no old-action slot.
        unsafe {
            libc::sigemptyset(&raw mut act.sa_mask);
            libc::sigaction(sig, &raw const act, std::ptr::null_mut());
        }
    }
}

/// A Muse Code session the stand-in keeps.
struct Session {
    dialect: Dialect,
    version: String,
    id: String,
    pid: u32,
    /// This process's start of the session (1 for a fresh one).
    start: usize,
    /// The lock, held open for the process's life (aterm's lane finds the session by it).
    _lock: Option<File>,
    log: Option<File>,
    seq: u64,
    runs: u64,
    transcript: Vec<String>,
    composer: Vec<u8>,
    /// An escape sequence being skipped.
    escape: Option<Vec<u8>>,
}

impl Session {
    fn open(dialect: Dialect, version: String, resume: Option<String>, cwd: &str) -> Session {
        let id = resume.filter(|r| is_uuid(r)).unwrap_or_else(new_uuid);
        let pid = std::process::id();
        let dir = sessions_root(dialect).map(|root| session_dir(&root, &id));
        let mut lock = None;
        let mut log = None;
        let mut start = 1;
        if let Some(dir) = &dir
            && std::fs::create_dir_all(dir).is_ok()
        {
            start = 1 + read_regular_text(&dir.join("session.jsonl"))
                .unwrap_or_default()
                .matches("\"payload_type\":\"session.started\"")
                .count();
            lock = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .read(true)
                .open(dir.join(".session.lock"))
                .ok();
            if let Some(f) = lock.as_mut() {
                let _ = f.write_all(format!("pid={pid}\n").as_bytes());
            }
            log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("session.jsonl"))
                .ok();
        }
        let mut s = Session {
            dialect,
            version,
            id,
            pid,
            start,
            _lock: lock,
            log,
            seq: 0,
            runs: 0,
            transcript: Vec::new(),
            composer: Vec::new(),
            escape: None,
        };
        s.record(
            "session.started",
            &format!(r#"{{"kind":"session_started","record":{{"pid":{pid}}}}}"#),
        );
        let cwd = json(cwd);
        s.record(
            "runtime.session.route_facts",
            &format!(
                r#"{{"kind":"route_facts","record":{{"cwd":"{cwd}","pid":{pid},"terminal_title_identity":"standin"}}}}"#
            ),
        );
        s.record(
            "runtime.session.metadata",
            &format!(r#"{{"kind":"metadata","record":{{"workspace_root":"{cwd}"}}}}"#),
        );
        s
    }

    fn record(&mut self, payload_type: &str, payload: &str) {
        self.seq += 1;
        let line = format!(
            r#"{{"schema_version":1,"stream":{{"kind":"session","id":"{}"}},"sequence":{},"recorded_at":{},"record_type":"event","durability":"durable","payload_type":"{payload_type}","payload_schema_version":1,"payload":{payload}}}"#,
            self.id,
            self.seq,
            now_us()
        );
        if let Some(f) = self.log.as_mut() {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
    }

    fn clean_end(&mut self) {
        self.record(
            "session.end",
            r#"{"kind":"session_end","record":{"exit_reason":"clean","uptime_ms":1}}"#,
        );
    }

    /// One turn on `prompt`, which ends at once.
    fn turn(&mut self, prompt: &str) {
        self.runs += 1;
        let run = format!("run-{}-{}", self.pid, self.runs);
        let p = json(prompt);
        self.record(
            "runtime.session",
            &format!(
                r#"{{"kind":"run","run_id":"{run}","event":{{"kind":"started","prompt":"{p}"}}}}"#
            ),
        );
        self.record(
            "runtime.session",
            &format!(
                r#"{{"kind":"run","run_id":"{run}","event":{{"kind":"terminal","terminal":"completed","reason":null}}}}"#
            ),
        );
        self.transcript.push(format!("❯ {prompt}"));
        self.transcript.push(String::new());
        self.transcript.push(format!(
            "◆ stand-in reply {} (start {})",
            self.runs, self.start
        ));
        self.transcript.push(String::new());
    }

    fn footer(&self) -> String {
        format!(
            "  standin · {} · pid {} · session {} · start {}",
            self.version, self.pid, self.id, self.start
        )
    }

    fn draw(&self) {
        let (cols, rows) = size();
        let rule = "─".repeat(cols);
        let fit = |s: &str| s.chars().take(cols).collect::<String>();
        let room = rows.saturating_sub(8);
        let skip = self.transcript.len().saturating_sub(room);
        let mut out = String::from("\x1b[H\x1b[2J\r\n");
        out.push_str(&fit(&format!(
            "  {} {}",
            self.dialect.banner(),
            self.version
        )));
        out.push_str("\r\n\r\n");
        for line in &self.transcript[skip..] {
            out.push_str(&fit(line));
            out.push_str("\r\n");
        }
        out.push_str(&rule);
        out.push_str("\r\n");
        out.push_str(&fit(&format!(
            "❯ {}",
            String::from_utf8_lossy(&self.composer)
        )));
        out.push_str("\r\n");
        out.push_str(&rule);
        out.push_str("\r\n");
        out.push_str(&fit(&self.footer()));
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }

    /// Take `bytes` of input; `Some(code)` when a command ends the process.
    fn input(&mut self, bytes: &[u8], mode: &Mode) -> Option<i32> {
        for &b in bytes {
            if let Some(seq) = self.escape.as_mut() {
                seq.push(b);
                let done =
                    (seq.len() > 1 && (0x40..=0x7e).contains(&b) && b != b'[') || seq.len() > 32;
                if done {
                    self.escape = None;
                }
                continue;
            }
            match b {
                0x1b => self.escape = Some(vec![b]),
                b'\r' | b'\n' => {
                    let text = String::from_utf8_lossy(&self.composer).trim().to_string();
                    self.composer.clear();
                    if let Some(code) = self.submit(&text, mode) {
                        return Some(code);
                    }
                }
                0x7f | 0x08 => {
                    let _ = self.composer.pop();
                }
                0x15 => self.composer.clear(),
                b if b >= 0x20 => self.composer.push(b),
                _ => {}
            }
        }
        None
    }

    fn submit(&mut self, text: &str, mode: &Mode) -> Option<i32> {
        if text.is_empty() {
            return None;
        }
        log(&format!(
            "submit pid={} session={} text={}",
            self.pid,
            self.id,
            word(text.as_bytes())
        ));
        if text == "/crash" {
            log(&format!("crash pid={} signal=SIGKILL", self.pid));
            aterm_uds::process::kill_self_now();
        }
        if let Some(code) = text.strip_prefix("/exit") {
            let code = code.trim().parse::<i32>().unwrap_or(0);
            self.clean_end();
            mode.restore();
            log(&format!("exit pid={} code={code}", self.pid));
            let _ = write!(std::io::stdout(), "\r\nstandin: exit {code}\r\n");
            return Some(code);
        }
        self.turn(text);
        None
    }
}

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let argv0 = argv.first().cloned().unwrap_or_default();
    let Some(dialect) = Dialect::of(&argv0) else {
        eprintln!(
            "aterm-lab-standin: run as a vendor's command (muse), not {}",
            Path::new(&argv0).display()
        );
        return ExitCode::from(2);
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(&argv0));
    let version = store_version(&exe).unwrap_or_else(|| "0.0.0".to_string());
    let args: Vec<&[u8]> = argv.iter().skip(1).map(|a| a.as_bytes()).collect();
    let pid = std::process::id();
    if args.iter().any(|a| *a == b"--help" || *a == b"-h") {
        log(&format!(
            "help pid={pid} exe={}",
            word(exe.as_os_str().as_bytes())
        ));
        println!(
            "{} {version} (aterm-lab stand-in)\n\nusage: muse [resume <session>]",
            dialect.banner()
        );
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| *a == b"--version" || *a == b"-V") {
        println!("{version} (aterm-lab stand-in)");
        return ExitCode::SUCCESS;
    }
    let resume = args
        .windows(2)
        .find(|w| w[0] == b"resume")
        .map(|w| String::from_utf8_lossy(w[1]).into_owned());
    let cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    install_handlers();
    let mut session = Session::open(dialect, version.clone(), resume, &cwd);
    // SAFETY: getppid and getpgrp take no arguments and cannot fail.
    let (ppid, pgid) = unsafe { (libc::getppid(), libc::getpgrp()) };
    log(&format!(
        "start pid={pid} ppid={ppid} pgid={pgid} exe={} version={version} session={} start={} argv={}",
        word(exe.as_os_str().as_bytes()),
        session.id,
        session.start,
        args.iter().map(|a| word(a)).collect::<Vec<_>>().join(",")
    ));
    let mode = Mode::raw();
    session.draw();
    let mut buf = [0u8; 4096];
    loop {
        if TERM.swap(false, Ordering::SeqCst) {
            log(&format!("signal pid={pid} TERM"));
            let _ = write!(std::io::stdout(), "\r\nstandin: got SIGTERM\r\n");
            session.clean_end();
            mode.restore();
            return ExitCode::from(143);
        }
        if HUP.swap(false, Ordering::SeqCst) {
            log(&format!("signal pid={pid} HUP"));
            let _ = write!(std::io::stdout(), "\r\nstandin: got SIGHUP\r\n");
            mode.restore();
            // SAFETY: back to the default action, then the same signal to this very process: it
            // ends by SIGHUP, which writes no core.
            unsafe {
                libc::signal(libc::SIGHUP, libc::SIG_DFL);
                libc::kill(libc::getpid(), libc::SIGHUP);
            }
            return ExitCode::from(129);
        }
        if INT.swap(false, Ordering::SeqCst) {
            log(&format!("signal pid={pid} INT"));
            session.transcript.push("standin: got SIGINT".to_string());
            session.draw();
        }
        if WINCH.swap(false, Ordering::SeqCst) {
            session.draw();
        }
        let mut fds = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll on one valid, owned pollfd.
        let ready = unsafe { libc::poll(&raw mut fds, 1, 200) };
        if ready <= 0 {
            continue;
        }
        if fds.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
            && fds.revents & libc::POLLIN == 0
        {
            log(&format!("eof pid={pid}"));
            return ExitCode::from(129);
        }
        // SAFETY: read(2) on fd 0 into the owned `buf`, at most its length. Unbuffered, so no byte
        // waits in this process where `poll` cannot see it.
        let got = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        let n = match usize::try_from(got) {
            Ok(0) => {
                log(&format!("eof pid={pid}"));
                return ExitCode::from(129);
            }
            Ok(n) => n,
            Err(_) => continue,
        };
        if let Some(code) = session.input(&buf[..n], &mode) {
            return ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1));
        }
        session.draw();
    }
}
