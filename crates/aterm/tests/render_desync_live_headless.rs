// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE: A RENDER DESYNC — a net-zero alternate-screen resize flap — read by
//! the surfaces that exist to see it (`status render=`, `resizes`, `cast
//! drift`, the screen itself) and repaired by the repaint nudge.
//!
//! THE INCIDENT. aterm re-gridded a window 64 -> 63 -> 64 rows within a
//! millisecond. On the alternate screen the shrink demotes the top row (the
//! alt grid keeps no history) and the grow appends a blank row, so the flap
//! moves the whole screen up one row. Claude Code repaints with ESC[2J only
//! when its SIGWINCH handler reads a CHANGED size; when both resizes land
//! before the handler runs it reads an unchanged size, skips the repaint, and
//! every later diff frame lands on shifted content. The engine now undoes a
//! flap with NO output between its halves; any output between them drops the
//! undo (even a mode set that draws nothing), and the ledger still reads one
//! net-zero run: that flap still shifts, and is what the surfaces judge.
//!
//! THE STAND-IN ([`Tui`], played by this binary through
//! `support/standin.rs`) paints a Claude-shaped frame on the alternate screen
//! (transcript rows, one of them truecolor, a rule, the composer, a rule, three
//! footer rows) and holds every SIGWINCH until it reads `g`, so both resizes
//! land before its handler reads the size: the order is the stand-in's, never
//! a clock's. The flap is two `resize` requests on one control connection
//! ([`Between`] says what lands between them, each seen landed before the
//! grow), and each variant is one test with an instance of its own.
//!
//! WHEN THE CLOCK IS PART OF THE SCENARIO. The ledger joins two resizes into
//! one run within its 250 ms run gap when the app DREW between them; without a
//! draw it joins them however late. So only the flap split by a drawn tick
//! depends on the gap, and that instance widens it through the development
//! seam `ATERM_RESIZE_RUN_GAP_MS` instead of racing it. The variants whose
//! halves must be FAR apart wait out a gap past the run gap (a lower bound,
//! which load only lengthens). That the nudge acts once and not again is
//! `aterm-core`'s `repaint_nudge` unit tests'.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use aterm_spec::wait::MUST_HAPPEN;

use crate::headless_boot::{self, Instance};
use crate::live_ctl::{Ctl, fields};
use crate::standin;

/// The real test that dispatches this lane's child app before its parent body.
const ENTRY: &str = "render_desync_live_headless::a_quiet_flap_is_undone";

/// `cast drift`'s run gap, equal to the ledger's (`aterm-gui`
/// `resize_ledger::RUN_GAP_MS`): two resizes farther apart, with a draw
/// between, are two runs to both.
const RUN_GAP_MS: f64 = aterm_control::cast_drift::RUN_GAP_SECS * 1000.0;

/// LATE's halves: past the run gap and the ledger's 1 s risk wait.
const LATE_GAP: Duration = Duration::from_millis(1_600);

/// The frame's truecolor row (screen row 3, 200,100,50 on 10,20,30).
const TRUECOLOR: (&str, &str) = ("c86432", "0a141e");

// ---- the stand-in ---------------------------------------------------------

/// How the stand-in answers the SIGWINCH it was holding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Repaint {
    /// Node's tty: a full repaint (ESC[2J) only when the size it reads
    /// changed, else a diff frame on the composer (Claude Code's contract).
    OnChange,
    /// A full repaint whatever it reads (the control).
    Always,
    /// When the size changed, a repaint by cursor addressing and erase-line
    /// on every row, NO ESC[2J: one the engine cannot tell from a diff.
    NoClear,
}

impl Repaint {
    fn word(self) -> &'static str {
        match self {
            Repaint::OnChange => "on-change",
            Repaint::Always => "always",
            Repaint::NoClear => "no-clear",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [Repaint::OnChange, Repaint::Always, Repaint::NoClear]
            .into_iter()
            .find(|r| r.word() == word)
    }
}

/// A SIGWINCH arrived since the stand-in last read its size.
static WINCH: AtomicBool = AtomicBool::new(false);
/// The write end of the stand-in's self-pipe, which wakes its `poll`.
static WAKE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_sigwinch(_: libc::c_int) {
    WINCH.store(true, Ordering::SeqCst);
    let fd = WAKE.load(Ordering::SeqCst);
    // SAFETY: an async-signal-safe write of one byte to a non-blocking pipe.
    unsafe {
        libc::write(fd, b"w".as_ptr().cast(), 1);
    }
}

/// The Claude-shaped alternate-screen app (see the module note). Its keys:
/// `m` a mode set that draws nothing (`?1004h`); `t` a spinner tick drawn on
/// the row it believes is its last, then the same mode set; `d` a diff line
/// drawn on the moved rows; `g` answer the SIGWINCH it was holding, if any,
/// and every one from here on; `q` (or end of input) exit. Every size it reads is appended
/// to `$HOME/standin.log` as `size=<cols>x<rows>`.
struct Tui {
    repaint: Repaint,
    /// The size it last read, `(rows, cols)`: what it lays its frames out for.
    seen: (u16, u16),
    log: Option<PathBuf>,
}

impl Tui {
    fn note(&self) {
        use std::io::Write as _;
        let Some(log) = &self.log else { return };
        let (rows, cols) = self.seen;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = writeln!(f, "size={cols}x{rows}");
        }
    }

    /// The whole frame for the size it last read, the composer holding
    /// `composer`; `clear` starts it with ESC[2J, else every row is erased
    /// to its end instead.
    fn frame(&self, composer: &str, clear: bool) -> String {
        let (rows, cols) = self.seen;
        let at = usize::from(rows) - 5;
        let mut lines: Vec<String> = (0..at - 1)
            .map(|r| format!("  ⏺ transcript line {r:02}"))
            .collect();
        lines[3] = "  \x1b[38;2;200;100;50;48;2;10;20;30m⏺ transcript line 03\x1b[0m".into();
        let rule = "─".repeat(usize::from(cols));
        lines.push(rule.clone());
        lines.push(format!("> {composer}"));
        lines.push(rule);
        lines.push("  ⏵⏵ bypass permissions on (shift+tab to cycle)".into());
        lines.push("  1 shell · esc to interrupt".into());
        lines.push("  ◯ context 12% · 1.5m tokens".into());
        let mut s = String::from("\x1b[?2026h");
        if clear {
            s.push_str("\x1b[2J");
        }
        for (r, text) in lines.iter().enumerate() {
            let erase = if clear { "" } else { "\x1b[K" };
            s.push_str(&format!("\x1b[{};1H{text}{erase}", r + 1));
        }
        s.push_str(&format!(
            "\x1b[{};{}H\x1b[?2026l",
            at + 1,
            3 + composer.len()
        ));
        s
    }

    /// Its SIGWINCH handler: read the size and repaint as [`Repaint`] says.
    fn on_winch(&mut self) {
        let now = standin::winsize();
        let changed = now != self.seen;
        self.seen = now;
        self.note();
        match self.repaint {
            Repaint::NoClear if changed => standin::put(self.frame("typed go", false).as_bytes()),
            Repaint::Always => standin::put(self.frame("typed go", true).as_bytes()),
            _ if changed => standin::put(self.frame("typed go", true).as_bytes()),
            _ => {
                let at = self.seen.0 - 5;
                standin::put(
                    format!("\x1b[?2026h\x1b[{};3Htyped go\x1b[K\x1b[?2026l", at + 1).as_bytes(),
                );
            }
        }
    }

    fn play(repaint: Repaint) -> ! {
        let mut pipe = [0; 2];
        // SAFETY: a pipe into a two-slot array, both ends made non-blocking,
        // and the SIGWINCH handler installed for the whole process.
        unsafe {
            assert_eq!(libc::pipe(pipe.as_mut_ptr()), 0, "pipe");
            for fd in pipe {
                libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
            }
            WAKE.store(pipe[1], Ordering::SeqCst);
            let mut sa: libc::sigaction = std::mem::zeroed();
            let handler: extern "C" fn(libc::c_int) = on_sigwinch;
            sa.sa_sigaction = handler as libc::sighandler_t;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut());
        }
        let mut tui = Tui {
            repaint,
            seen: standin::winsize(),
            log: std::env::var_os("HOME").map(|h| Path::new(&h).join("standin.log")),
        };
        tui.note();
        standin::raw();
        standin::put(format!("\x1b[?1049h{}", tui.frame("", true)).as_bytes());
        let mut answering = false;
        loop {
            let mut fds = [
                libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pipe[0],
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: two valid `pollfd`s.
            if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
                continue;
            }
            if fds[1].revents != 0 {
                let mut drain = [0u8; 64];
                // SAFETY: a read of the non-blocking pipe into a live buffer.
                while unsafe { libc::read(pipe[0], drain.as_mut_ptr().cast(), drain.len()) } > 0 {}
            }
            if answering && WINCH.swap(false, Ordering::SeqCst) {
                tui.on_winch();
            }
            if fds[0].revents == 0 {
                continue;
            }
            let mut key = 0u8;
            // SAFETY: a one-byte read into a live local.
            if unsafe { libc::read(0, (&raw mut key).cast(), 1) } != 1 {
                standin::exit(0);
            }
            match key {
                b'm' => standin::put(b"\x1b[?1004h"),
                b't' => standin::put(
                    format!(
                        "\x1b[?2026h\x1b[{};3Htick\x1b[K\x1b[?2026l\x1b[?1004h",
                        tui.seen.0
                    )
                    .as_bytes(),
                ),
                b'd' => standin::put(
                    format!(
                        "\x1b[?2026h\x1b[{};3Hdrawn frame\x1b[K\x1b[?2026l",
                        tui.seen.0 - 4
                    )
                    .as_bytes(),
                ),
                b'g' => {
                    answering = true;
                    if WINCH.swap(false, Ordering::SeqCst) {
                        tui.on_winch();
                    }
                }
                b'q' => standin::exit(0),
                _ => {}
            }
        }
    }
}

/// Play the [`Tui`] whose [`Repaint`] is named in [`standin::ENV`], or return
/// to the parent test.
fn play_standin_if_asked() {
    let Some(spec) = standin::spec() else { return };
    match Repaint::parse(&spec) {
        Some(repaint) => Tui::play(repaint),
        None => {
            standin::put(format!("unknown repaint: {spec}\r\n").as_bytes());
            standin::exit(2)
        }
    }
}

// ---- the lane --------------------------------------------------------------

/// What one instance is started with.
#[derive(Clone, Copy)]
struct Setup {
    repaint: Repaint,
    /// `ATERM_NUDGE_CLASS=agent`: the stand-in reads as Claude Code.
    agent: bool,
    /// `[render] repaint_nudge`; `None` is the compiled default (`on`).
    nudge: Option<&'static str>,
    /// `ATERM_RESIZE_RUN_GAP_MS`: the ledger's run gap, widened.
    run_gap: Option<Duration>,
    /// `aterm.log` is written (the isolation turns it off).
    log: bool,
}

impl Setup {
    const fn new(repaint: Repaint) -> Self {
        Self {
            repaint,
            agent: false,
            nudge: None,
            run_gap: None,
            log: false,
        }
    }

    /// The epoch judge's setup: the agent class, and the nudge off, which
    /// would repair the very screen the test reads.
    const fn epoch() -> Self {
        Self {
            agent: true,
            nudge: Some("off"),
            ..Self::new(Repaint::OnChange)
        }
    }
}

/// What lands between the halves of the flap, each seen landed before the
/// grow.
#[derive(Clone, Copy, Debug)]
enum Between {
    /// Nothing: the incident's flap, which the engine's resize undo answers.
    Nothing,
    /// A mode set that draws nothing (`?1004h`, seen in `modes`), then the
    /// grow this long after it.
    ModeSet(Duration),
    /// A spinner tick drawn on the footer, then the same mode set.
    Tick,
    /// An app frame drawn on the moved rows, seen on screen, then the grow
    /// this long after it.
    Frame(Duration),
}

/// One reply line `<kind> [<id>] k=v…`.
#[derive(Clone, Debug)]
struct Rec {
    kind: String,
    id: Option<String>,
    fields: BTreeMap<String, String>,
}

impl Rec {
    fn parse(line: &str) -> Self {
        let mut toks = line.split_whitespace();
        let kind = toks.next().unwrap_or_default().to_owned();
        let rest: Vec<&str> = toks.collect();
        let id = rest
            .first()
            .filter(|t| !t.contains('='))
            .map(|t| (*t).to_owned());
        Self {
            kind,
            id,
            fields: fields(line)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    /// Every `(key, value)` in `want` is this record's.
    #[track_caller]
    fn expect(&self, what: &str, want: &[(&str, &str)]) {
        let off: Vec<_> = want
            .iter()
            .filter(|(k, v)| self.get(k) != Some(*v))
            .collect();
        assert!(off.is_empty(), "{what}: want {off:?} in {self:?}");
    }
}

/// A `resizes` reply.
#[derive(Debug)]
struct Resizes {
    header: Rec,
    rows: Vec<Rec>,
    runs: Vec<Rec>,
    epochs: Vec<Rec>,
}

impl Resizes {
    /// The flap's two rows and the one run that holds them both.
    #[track_caller]
    fn one_flap_run(&self) -> (&Rec, &Rec, &Rec) {
        let [r1, r2] = self.rows.as_slice() else {
            panic!("the ledger books the flap's two resizes: {self:#?}");
        };
        let [run] = self.runs.as_slice() else {
            panic!("the flap is one run: {self:#?}");
        };
        run.expect(
            "the flap's run",
            &[
                ("n", "2"),
                ("alt", "1"),
                ("geom", "80x24>80x23>80x24"),
                ("net", "zero"),
            ],
        );
        let id = run.id.as_deref();
        assert!(
            r1.get("run") == id && r2.get("run") == id,
            "both resizes are the run's: {self:#?}"
        );
        (run, r1, r2)
    }

    /// Both resizes were booked at the controller's input dispatch: `site=input`,
    /// `at=` a line of this checkout that calls `input_to_session(` (or its
    /// receipt-taking `input_to_session_observed(`), nothing
    /// unledgered, and each row's window the size it asked for.
    #[track_caller]
    fn expect_site_input(&self, r1: &Rec, r2: &Rec) {
        self.header
            .expect("nothing unledgered", &[("unledgered", "0")]);
        r1.expect("the shrink's site", &[("site", "input"), ("win", "80x23")]);
        r2.expect("the grow's site", &[("site", "input"), ("win", "80x24")]);
        let at = r1.get("at").unwrap_or_default();
        assert_eq!(r2.get("at"), Some(at), "one caller for both: {r1:?} {r2:?}");
        let (file, line) = at.rsplit_once(':').expect("at=<file>:<line>");
        let line: usize = line.parse().expect("a line number");
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|e| panic!("at={at} names no file of this checkout: {e}"));
        let text = source.lines().nth(line - 1).unwrap_or_default();
        assert!(
            text.contains("input_to_session(") || text.contains("input_to_session_observed("),
            "at={at} is the controller-input dispatch, but reads: {text}"
        );
    }

    fn last_epoch(&self) -> &Rec {
        self.epochs
            .last()
            .unwrap_or_else(|| panic!("an epoch row: {self:#?}"))
    }
}

/// The flap's shrink demoted the top row and the output between its halves
/// dropped the undo, so the grow appended one: the run is one row up.
#[track_caller]
fn expect_displaced(r1: &Rec, r2: &Rec, run: &Rec) {
    r1.expect(
        "the shrink demoted the top row",
        &[("demoted", "1"), ("shift", "-1")],
    );
    r2.expect(
        "the grow appended one",
        &[("appended", "1"), ("restored", "0"), ("shift", "+0")],
    );
    run.expect("the run is one row up", &[("displaced", "-1")]);
}

/// A `cast drift` reply.
#[derive(Debug)]
struct Drift {
    header: Rec,
    lines: Vec<Rec>,
}

impl Drift {
    #[track_caller]
    fn expect_clean(&self) {
        self.header.expect(
            "cast drift is clean",
            &[
                ("verdict", "clean"),
                ("fidelity", "pass"),
                ("drift_rows", "0"),
            ],
        );
    }

    /// `verdict=desync fidelity=pass`, its culprit the flap: the culprit run's
    /// `t=` is the header's `culprit=`, a net-zero 24 -> 23 -> 24 run. The
    /// culprit run's line is returned.
    #[track_caller]
    fn expect_names_the_flap(&self) -> &Rec {
        self.header.expect(
            "cast drift names the flap",
            &[("verdict", "desync"), ("fidelity", "pass")],
        );
        let run = self
            .lines
            .iter()
            .find(|r| r.kind == "run" && r.get("culprit") == Some("1"))
            .unwrap_or_else(|| panic!("a culprit run: {self:#?}"));
        assert_eq!(run.get("t"), self.header.get("culprit"), "{self:#?}");
        run.expect(
            "the culprit is the flap",
            &[("geom", "80x24>80x23>80x24"), ("net", "zero")],
        );
        run
    }

    /// Screen row `n`'s line, when it drifted.
    fn row(&self, n: u16) -> Option<&Rec> {
        let n = n.to_string();
        self.lines
            .iter()
            .find(|r| r.kind == "row" && r.id.as_deref() == Some(n.as_str()))
    }
}

/// One instance whose only session is the stand-in.
struct Lane {
    inst: Instance,
    ctl: Ctl,
}

impl Lane {
    /// Boot it (80x24) and wait for the stand-in's frame; an environment
    /// refusal is COULD NOT RUN, never a pass ([`headless_boot::boot_with`]).
    #[track_caller]
    fn boot(tag: &str, setup: Setup) -> Self {
        let inst = headless_boot::boot_with(
            &format!("atrd{tag}"),
            |root, cmd| {
                if let Some(word) = setup.nudge {
                    let config = root.join("cfg/aterm/aterm.toml");
                    let mut text = std::fs::read_to_string(&config).expect("the scratch config");
                    text.push_str(&format!("[render]\nrepaint_nudge = \"{word}\"\n"));
                    std::fs::write(&config, text).expect("write the scratch config");
                }
                if setup.agent {
                    cmd.env("ATERM_NUDGE_CLASS", "agent");
                }
                if let Some(gap) = setup.run_gap {
                    cmd.env("ATERM_RESIZE_RUN_GAP_MS", gap.as_millis().to_string());
                }
                if setup.log {
                    cmd.env_remove("ATERM_LOG");
                }
                cmd.env(standin::ENV, setup.repaint.word())
                    .args(["--columns", "80", "--lines", "24", "-e"])
                    .arg(standin::exe())
                    .args(standin::argv(ENTRY));
            },
            |_| true,
        );
        let mut ctl = Ctl::connect(&inst.sock);
        ctl.expect_on_screen("bypass.permissions");
        Self { inst, ctl }
    }

    /// `cell <row> <col>`'s colours, `(fg, bg)`.
    fn colours(&mut self, row: u16, col: u16) -> (String, String) {
        let reply = self.ctl.ok(&format!("cell {row} {col}"));
        let mut toks = reply.split_whitespace().skip(2);
        let fg = toks.next().unwrap_or_default().to_owned();
        let bg = toks.next().unwrap_or_default().to_owned();
        (fg, bg)
    }

    /// The flap: `resize 23 80`, then `between`, then `resize 24 80`.
    fn flap(&mut self, between: Between) {
        self.ctl.ok("resize 23 80");
        let after = match between {
            Between::Nothing => Duration::ZERO,
            Between::ModeSet(after) => {
                self.ctl.send("m");
                self.await_focus_reporting();
                after
            }
            Between::Tick => {
                self.ctl.send("t");
                self.await_focus_reporting();
                Duration::ZERO
            }
            Between::Frame(after) => {
                self.ctl.send("d");
                self.ctl.expect_on_screen("drawn.frame");
                after
            }
        };
        // The scenario's gap between the halves (a lower bound), never a wait
        // for something to land: that was seen above.
        if !after.is_zero() {
            std::thread::sleep(after);
        }
        self.ctl.ok("resize 24 80");
    }

    /// The stand-in's mode set has been parsed: so has everything before it.
    fn await_focus_reporting(&mut self) {
        aterm_spec::wait::until_within(MUST_HAPPEN, "the stand-in's mode set to apply", || {
            (self.ctl.modes().get("focus_reporting").map(String::as_str) == Some("true"))
                .then_some(())
        });
    }

    /// The truecolor row kept its colours across the flap: it is at `row`
    /// now (3 after an undone flap, 2 one row up), read before the stand-in
    /// may repaint, so the colours are the engine's own.
    #[track_caller]
    fn expect_truecolor_at(&mut self, before: &(String, String), row: u16) {
        assert_eq!(
            (before.0.as_str(), before.1.as_str()),
            TRUECOLOR,
            "the frame's truecolor row before the flap"
        );
        assert_eq!(
            &self.colours(row, 4),
            before,
            "the truecolor row at {row} after the flap"
        );
    }

    /// Let the stand-in's handler read the size (`g`) and wait for its draw.
    fn unblock(&mut self) {
        self.ctl.send("g");
        self.ctl.expect_on_screen("typed.go");
    }

    fn render(&mut self) -> String {
        let status = self.ctl.status();
        fields(&status).get("render").unwrap_or(&"").to_string()
    }

    /// Wait (a hang detector) for `status render=<want>`.
    #[track_caller]
    fn await_render(&mut self, want: &str) {
        let mut last = String::new();
        let seen = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(20), || {
            last = self.render();
            (last == want).then_some(())
        });
        assert!(seen.is_some(), "status render={want}: still render={last}");
    }

    fn resizes(&mut self) -> Resizes {
        let (header, body) = self.ctl.counted("resizes");
        let lines: Vec<Rec> = body.iter().map(|l| Rec::parse(l)).collect();
        let of =
            |kind: &str| -> Vec<Rec> { lines.iter().filter(|r| r.kind == kind).cloned().collect() };
        Resizes {
            header: Rec::parse(&header),
            rows: of("resize"),
            runs: of("run"),
            epochs: of("epoch"),
        }
    }

    /// Wait (a hang detector) until the `resizes` reply holds `done`; return it.
    #[track_caller]
    fn await_resizes(&mut self, what: &str, done: impl Fn(&Resizes) -> bool) -> Resizes {
        let mut last = None;
        let seen = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(20), || {
            let rz = self.resizes();
            let hit = done(&rz);
            last = Some(rz);
            hit.then_some(())
        });
        let rz = last.expect("one reading");
        assert!(seen.is_some(), "{what}: {rz:#?}");
        rz
    }

    fn drift(&mut self) -> Drift {
        let (header, body) = self.ctl.counted("cast drift");
        Drift {
            header: Rec::parse(&header),
            lines: body.iter().map(|l| Rec::parse(l)).collect(),
        }
    }

    /// The screen's top and bottom rows.
    fn top_and_bottom(&mut self) -> (String, String) {
        let text = self.ctl.text();
        (
            text.first().cloned().unwrap_or_default(),
            text.get(23).cloned().unwrap_or_default(),
        )
    }

    /// The frame sits one row up: row 0 is painted row 1, row 23 blank.
    #[track_caller]
    fn expect_shifted(&mut self) {
        let (top, bottom) = self.top_and_bottom();
        assert!(
            top.contains("transcript line 01") && bottom.trim().is_empty(),
            "the frame shifted up one row: row 0={top:?} row 23={bottom:?}"
        );
    }

    /// The frame sits where it was painted: row 0 is painted row 0, row 23
    /// the footer.
    #[track_caller]
    fn expect_unshifted(&mut self) {
        let (top, bottom) = self.top_and_bottom();
        assert!(
            top.contains("transcript line 00") && bottom.contains("context 12%"),
            "the frame where it was painted: row 0={top:?} row 23={bottom:?}"
        );
    }

    /// The sizes the stand-in READ, in order (`80x24`, …).
    fn sizes_read(&self) -> Vec<String> {
        std::fs::read_to_string(self.inst.tmp.join("home/standin.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("size=").map(str::to_owned))
            .collect()
    }

    /// Wait (a hang detector) until the stand-in has read the nudge's short
    /// size and then the true one: the hold reached the PTY, and so did its
    /// release.
    #[track_caller]
    fn await_short_then_true_read(&self) {
        let seen = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(20), || {
            self.sizes_read()
                .ends_with(&["80x23".into(), "80x24".into()])
                .then_some(())
        });
        assert!(
            seen.is_some(),
            "the stand-in read the short size, then the true one: {:?}",
            self.sizes_read()
        );
    }

    /// The instance's `aterm.log`, wherever under its scratch world it is.
    fn aterm_log(&self) -> String {
        fn find(dir: &Path) -> Option<PathBuf> {
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(found) = find(&path) {
                        return Some(found);
                    }
                } else if path.file_name().is_some_and(|n| n == "aterm.log") {
                    return Some(path);
                }
            }
            None
        }
        find(&self.inst.tmp)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default()
    }
}

/// THE INCIDENT'S FLAP, on this engine: nothing between the halves, so the
/// grow hands the demoted row back. One net-zero run that moved nothing
/// (`none`, `render=ok` for good), `cast drift` clean, the screen unshifted,
/// the truecolor row where it was.
#[test]
fn a_quiet_flap_is_undone() {
    play_standin_if_asked();
    let mut lane = Lane::boot("q", Setup::new(Repaint::OnChange));
    let before = lane.colours(3, 4);
    lane.flap(Between::Nothing);
    lane.expect_truecolor_at(&before, 3);
    lane.unblock();
    let rz = lane.resizes();
    let (run, r1, r2) = rz.one_flap_run();
    r1.expect(
        "the shrink demoted the top row",
        &[("demoted", "1"), ("shift", "-1")],
    );
    r2.expect(
        "the grow handed it back",
        &[
            ("revealed", "1"),
            ("restored", "1"),
            ("appended", "0"),
            ("shift", "+1"),
        ],
    );
    run.expect(
        "the run moved nothing",
        &[("displaced", "+0"), ("verdict", "none")],
    );
    rz.expect_site_input(r1, r2);
    assert_eq!(lane.render(), "ok");
    lane.drift().expect_clean();
    lane.expect_unshifted();
}

/// A mode set between the halves drops the undo; the handler then reads an
/// unchanged size and sends a diff, as Node does. `status` says
/// `desync-risk`, `resizes` one net-zero run one row up judged
/// `desync-risk`, `cast drift` names the flap with the footer where the screen
/// has a blank row, and the screen is one row up.
#[test]
fn a_split_flap_the_app_drew_over_is_a_desync_risk() {
    let mut lane = Lane::boot("d", Setup::new(Repaint::OnChange));
    let before = lane.colours(3, 4);
    lane.flap(Between::ModeSet(Duration::ZERO));
    lane.expect_truecolor_at(&before, 2);
    lane.unblock();
    lane.await_render("desync-risk");
    let rz = lane.resizes();
    let (run, r1, r2) = rz.one_flap_run();
    expect_displaced(r1, r2, run);
    run.expect("the run's verdict", &[("verdict", "desync-risk")]);
    rz.expect_site_input(r1, r2);
    let drift = lane.drift();
    drift.expect_names_the_flap();
    let bottom = drift.row(23).expect("row 23 drifted");
    assert!(
        bottom.get("live") == Some("")
            && bottom
                .get("expected")
                .is_some_and(|e| e.contains("context")),
        "row 23: blank on screen, the footer in the replay: {bottom:?}"
    );
    lane.expect_shifted();
}

/// A spinner TICK drawn between the halves (the presence row's attention flap
/// with the app's spinner ticking): the draw drops the undo as the mode set
/// does, and the ledger keeps it inside the run — a draw is no sign the
/// handler read the smaller size — so everything reads as the split flap,
/// with the tick, not the footer, the replay's bottom row. The draw joins the
/// halves only within the run gap, so this instance widens the gap to a
/// minute: the halves are always inside it.
#[test]
fn a_flap_split_by_a_drawn_tick_is_one_run_and_a_desync_risk() {
    let setup = Setup {
        run_gap: Some(MUST_HAPPEN),
        ..Setup::new(Repaint::OnChange)
    };
    let mut lane = Lane::boot("t", setup);
    let before = lane.colours(3, 4);
    lane.flap(Between::Tick);
    lane.expect_truecolor_at(&before, 2);
    lane.unblock();
    lane.await_render("desync-risk");
    let rz = lane.resizes();
    let (run, r1, r2) = rz.one_flap_run();
    expect_displaced(r1, r2, run);
    run.expect("the run's verdict", &[("verdict", "desync-risk")]);
    rz.expect_site_input(r1, r2);
    let drift = lane.drift();
    drift.expect_names_the_flap();
    let bottom = drift.row(23).expect("row 23 drifted");
    assert!(
        bottom.get("live") == Some("")
            && bottom.get("expected").is_some_and(|e| e.contains("tick")),
        "row 23: blank on screen, the tick in the replay: {bottom:?}"
    );
    lane.expect_shifted();
}

/// THE CONTROL: the split flap, and a handler that repaints with ESC[2J
/// whatever it reads. The run is `healed`, `render=ok` for good, `cast drift`
/// clean, the screen unshifted.
#[test]
fn a_split_flap_the_app_repaints_is_healed() {
    let mut lane = Lane::boot("h", Setup::new(Repaint::Always));
    let before = lane.colours(3, 4);
    lane.flap(Between::ModeSet(Duration::ZERO));
    lane.expect_truecolor_at(&before, 2);
    lane.unblock();
    let rz = lane.await_resizes("the run is healed", |rz| {
        rz.runs.iter().any(|r| r.get("verdict") == Some("healed"))
    });
    let (run, r1, r2) = rz.one_flap_run();
    expect_displaced(r1, r2, run);
    rz.expect_site_input(r1, r2);
    assert_eq!(lane.render(), "ok");
    lane.drift().expect_clean();
    lane.expect_unshifted();
}

/// THE SPLIT FLAP WITH ITS HALVES 1.6 s APART: nothing was drawn between
/// them, so the shrink's run, displaced and undrawn, waits for the grow
/// however late — ONE net-zero run one row up, `desync-risk` once drawn on —
/// and `cast drift` weighs the span from the shrink to the grow as one return
/// and names it.
#[test]
fn a_split_flap_whose_halves_are_far_apart_is_still_one_run() {
    let mut lane = Lane::boot("l", Setup::new(Repaint::OnChange));
    lane.flap(Between::ModeSet(LATE_GAP));
    let rz = lane.resizes();
    let t = |r: &Rec| r.get("t").and_then(|t| t.parse::<f64>().ok());
    match rz.rows.as_slice() {
        [r1, r2] if t(r2).zip(t(r1)).is_some_and(|(b, a)| b - a > RUN_GAP_MS) => {}
        _ => panic!("the two resizes booked past the run gap: {rz:#?}"),
    }
    lane.unblock();
    lane.await_render("desync-risk");
    let rz = lane.resizes();
    let (run, r1, r2) = rz.one_flap_run();
    expect_displaced(r1, r2, run);
    run.expect("the run's verdict", &[("verdict", "desync-risk")]);
    let drift = lane.drift();
    let culprit = drift.expect_names_the_flap();
    let gap: f64 = culprit
        .get("gap_ms")
        .and_then(|g| g.parse().ok())
        .expect("the culprit's gap_ms=");
    assert!(gap > RUN_GAP_MS, "the return spans the halves: {culprit:?}");
    lane.expect_shifted();
}

/// THE EPOCH JUDGE, the run judge's blind spot: an app frame between the
/// halves and the grow `after` it, past the run gap, so the run judge sees
/// two net-changed runs; the engine's render debt, judged as one epoch, is a
/// `desync-risk` for the agent class.
fn a_flap_drawn_between_is_judged_by_its_epoch(tag: &str, after: Duration) {
    let mut lane = Lane::boot(tag, Setup::epoch());
    lane.flap(Between::Frame(after));
    lane.unblock();
    lane.await_render("desync-risk");
    let rz = lane.resizes();
    rz.last_epoch().expect(
        "the epoch",
        &[
            ("geom", "80x24>80x24"),
            ("shift", "-1"),
            ("pushed", "0"),
            ("drew", "1"),
            ("class", "agent"),
            ("verdict", "desync-risk"),
        ],
    );
    assert!(
        rz.runs.len() == 2 && rz.runs.iter().all(|r| r.get("net") == Some("changed")),
        "the run judge saw two net-changed runs: {rz:#?}"
    );
    lane.drift().header.expect(
        "cast drift names the flap",
        &[("verdict", "desync"), ("fidelity", "pass")],
    );
    lane.expect_shifted();
}

#[test]
fn a_flap_drawn_between_and_grown_300ms_later_is_judged_by_its_epoch() {
    a_flap_drawn_between_is_judged_by_its_epoch("e3", Duration::from_millis(300));
}

/// Past the ledger's 1 s risk wait: the shrink's half is judged before the
/// grow comes.
#[test]
fn a_flap_drawn_between_and_grown_1200ms_later_is_judged_by_its_epoch() {
    a_flap_drawn_between_is_judged_by_its_epoch("e12", Duration::from_millis(1_200));
}

/// The split flap and then SILENCE: the stand-in never reads the size and
/// nothing is drawn; the epoch alone reads `desync-risk` at the ledger's risk
/// wait (`drew=0`).
#[test]
fn an_undrawn_flap_is_judged_by_its_epoch() {
    let mut lane = Lane::boot("u", Setup::epoch());
    lane.flap(Between::ModeSet(Duration::ZERO));
    lane.await_render("desync-risk");
    let rz = lane.resizes();
    rz.last_epoch().expect(
        "the epoch",
        &[
            ("geom", "80x24>80x24"),
            ("shift", "-1"),
            ("pushed", "0"),
            ("drew", "0"),
            ("class", "agent"),
            ("verdict", "desync-risk"),
        ],
    );
    lane.drift().header.expect(
        "cast drift names the flap",
        &[("verdict", "desync"), ("fidelity", "pass")],
    );
    lane.expect_shifted();
}

/// THE REPAIR at the compiled default (no `[render]` key): after the drawn
/// flap aterm sets the PTY one row short and back, the stand-in reads the
/// short size and then the true one and repaints, and the screen is whole
/// again. The epoch closed `healed by=nudge`, the engine booked no resize,
/// and the PTY ends at the engine's size.
#[test]
fn the_repaint_nudge_repairs_a_desynced_agent() {
    let mut lane = Lane::boot(
        "n",
        Setup {
            agent: true,
            ..Setup::new(Repaint::OnChange)
        },
    );
    lane.flap(Between::Frame(Duration::ZERO));
    lane.unblock();
    let total = lane.resizes().header.get("total").map(str::to_owned);
    let rz = lane.await_resizes("the repair ran once and the nudge is idle again", |rz| {
        rz.header.get("nudged") == Some("1") && rz.header.get("nudge") == Some("idle")
    });
    rz.last_epoch().expect(
        "the epoch closed healed by the nudge",
        &[("verdict", "healed"), ("by", "nudge"), ("tried", "1")],
    );
    assert_eq!(
        rz.header.get("total"),
        total.as_deref(),
        "the repair booked no resize"
    );
    // The stand-in's repaint at the true size, the last thing it does.
    aterm_spec::wait::until_within(MUST_HAPPEN, "the repaint at the true size", || {
        let (top, bottom) = lane.top_and_bottom();
        (top.contains("transcript line 00") && bottom.contains("context 12%")).then_some(())
    });
    let sizes = lane.sizes_read();
    assert!(
        sizes.ends_with(&["80x23".into(), "80x24".into()]),
        "the stand-in read the short size, then the true one: {sizes:?}"
    );
    let dims = lane.ctl.ok("dims");
    assert_eq!(fields(&dims).get("composed_rows"), Some(&"24"), "{dims}");
    lane.drift().expect_clean();
}

/// `repaint_nudge = "log"`: aterm writes `would-nudge` to `aterm.log` and
/// never touches the PTY: the stand-in never reads a short size and the
/// screen stays one row up.
#[test]
fn the_repaint_nudge_in_log_mode_only_logs() {
    let mut lane = Lane::boot(
        "g",
        Setup {
            agent: true,
            nudge: Some("log"),
            log: true,
            ..Setup::new(Repaint::OnChange)
        },
    );
    lane.flap(Between::Frame(Duration::ZERO));
    lane.unblock();
    lane.await_render("desync-risk");
    let mut log = String::new();
    let logged = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(20), || {
        log = lane.aterm_log();
        log.contains("would-nudge").then_some(())
    });
    assert!(
        logged.is_some(),
        "aterm.log gets a would-nudge line:\n{log}"
    );
    let rz = lane.resizes();
    rz.header.expect("nothing was held", &[("nudged", "0")]);
    let sizes = lane.sizes_read();
    assert!(
        !sizes.iter().any(|s| s == "80x23"),
        "the PTY was never short: {sizes:?}"
    );
    let (top, _) = lane.top_and_bottom();
    assert!(
        top.contains("transcript line 01"),
        "still one row up: {top:?}"
    );
}

/// THE CONTAINMENT, not the agent class: a program that repaints by cursor
/// addressing (no ESC[2J) is indistinguishable from one that missed the
/// resize, so it is never accused (`render=unjudged`) and never touched.
#[test]
fn a_program_that_repaints_without_clearing_is_never_nudged() {
    let mut lane = Lane::boot(
        "p",
        Setup {
            nudge: Some("on"),
            ..Setup::new(Repaint::NoClear)
        },
    );
    lane.flap(Between::ModeSet(Duration::ZERO));
    lane.await_render("unjudged");
    let rz = lane.resizes();
    rz.header.expect(
        "the nudge never acted",
        &[("nudged", "0"), ("nudge", "idle")],
    );
}

/// THE CONTAINMENT, classed as the agent: the nudge tries ONCE, gets no full
/// repaint back, gives up and stays given up; no resize is booked and the PTY
/// ends at the engine's size.
#[test]
fn a_nudge_without_an_answer_gives_up_once() {
    let mut lane = Lane::boot(
        "a",
        Setup {
            agent: true,
            nudge: Some("on"),
            ..Setup::new(Repaint::NoClear)
        },
    );
    lane.flap(Between::Frame(Duration::ZERO));
    lane.unblock();
    let total = lane.resizes().header.get("total").map(str::to_owned);
    lane.await_resizes("the nudge tried once and is idle again", |rz| {
        rz.header.get("nudged") == Some("1") && rz.header.get("nudge") == Some("idle")
    });
    lane.await_short_then_true_read();
    let rz = lane.resizes();
    rz.header
        .expect("tried once", &[("nudged", "1"), ("nudge", "idle")]);
    rz.last_epoch().expect(
        "the epoch stays at risk, tried",
        &[("tried", "1"), ("verdict", "desync-risk")],
    );
    assert_eq!(
        rz.header.get("total"),
        total.as_deref(),
        "no resize was booked"
    );
    assert_eq!(
        lane.sizes_read().last().map(String::as_str),
        Some("80x24"),
        "the PTY ends at the engine's size"
    );
}
