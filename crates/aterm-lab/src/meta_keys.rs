// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `meta-keys --window`: Option as Meta, end to end through aterm's own window
//! (ATERM-KI-1015-9). It OPENS A WINDOW on the desktop of whoever runs it, so it
//! runs only when `--window` says so, never from a check.
//!
//! One private windowed aterm runs [`keydump_main`] in its tab, which prints the
//! bytes each key press reaches the program as. The lane raises the window (`raise
//! <sid>`, so the instance is the active app and its window key) and presses Option
//! chords through `hwkey … shape=press`: each event is built from a CoreGraphics
//! keyboard event, as a key press is, posted into the instance's own event queue, so
//! AppKit's `sendEvent:`, the window's `keyDown:` and its input method see what they
//! see for a physical key on the machine's layout. Enter ends each case, pressed
//! twice: an input method that composes takes the first Enter as its commit (a
//! dead key left pending is committed by it, and the Enter itself goes nowhere),
//! and the dump drops a case that is a lone Enter.
//!
//! * `C` (the control, Meta off: `settings set option_as_meta false`): Option+e then
//!   e composes `é`, and Option+b types `∫`. Only if the input method composed here
//!   does a Meta row's "no composition" mean anything; the control runs before and
//!   after the Meta rows, so the instance held the keyboard throughout.
//! * `M` (Meta on, the default): each U.S. Option dead key (e, u, i, n, the grave
//!   key) goes out as ESC and the bare letter, Option+b as ESC `b` (not ESC `∫`), and
//!   Option+e then e as ESC `e` `e` (no dead key left pending).
//!
//! The rows read the U.S. layout's characters. On another layout the lane says
//! NOT RUN: what each key types there is that layout's.
//!
//! A person at the desk is never typed over. The lane raises the window only
//! after the keyboard, mouse and trackpad have been idle for [`IDLE`] (the
//! kernel's `HIDIdleTime`), and a case during which a person touched any of them,
//! or whose `hwkey` says the window was not the key window of the active app
//! (`key=0`), is NOT MEASURED, never judged: its bytes may be theirs, and an
//! inactive app's input method composes nothing.
//!
//! macOS, a logged-in GUI session. Exit 0 every row passed with the control
//! composing, 1 a row failed, 2 usage, 3 not run or not measured (no GUI session, no
//! aterm, or the control did not compose), said on a `NOT RUN:` line.

use std::time::Duration;

use crate::instance::{self, CONFIG_OFF, Instance, Launch, World};
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

/// The ready line the dump prints once its terminal is raw.
const READY: &str = "key-dump ready";

/// How long no person may have touched the keyboard or pointer before the lane
/// takes the keyboard.
const IDLE: Duration = Duration::from_secs(3);

/// How long the lane waits for that quiet: a hang detector, never a budget.
const IDLE_WAIT: Duration = Duration::from_mins(5);

/// One case: the `hwkey` presses before its Enter, and the bytes the program must
/// read for them (the Enter's `0d` left out).
struct Case {
    what: &'static str,
    keys: &'static [&'static str],
    want: &'static [u8],
}

/// Meta off: what the input method composes on the U.S. layout.
const CONTROL: [Case; 2] = [
    Case {
        what: "Option+e then e composes é",
        keys: &["e mods=alt", "e"],
        want: "é".as_bytes(),
    },
    Case {
        what: "Option+b types ∫",
        keys: &["b mods=alt"],
        want: "∫".as_bytes(),
    },
];

/// Meta on: ESC and the key's bare letter, never a composition.
const META: [Case; 7] = [
    Case {
        what: "Option+e (dead acute) goes out as ESC e",
        keys: &["e mods=alt"],
        want: b"\x1be",
    },
    Case {
        what: "Option+u (dead umlaut) goes out as ESC u",
        keys: &["u mods=alt"],
        want: b"\x1bu",
    },
    Case {
        what: "Option+i (dead circumflex) goes out as ESC i",
        keys: &["i mods=alt"],
        want: b"\x1bi",
    },
    Case {
        what: "Option+n (dead tilde) goes out as ESC n",
        keys: &["n mods=alt"],
        want: b"\x1bn",
    },
    Case {
        what: "Option+` (dead grave) goes out as ESC `",
        keys: &["` mods=alt"],
        want: b"\x1b`",
    },
    Case {
        what: "Option+b goes out as ESC b, not ESC ∫",
        keys: &["b mods=alt"],
        want: b"\x1bb",
    },
    Case {
        what: "Option+e then e is ESC e e: no dead key pending",
        keys: &["e mods=alt", "e"],
        want: b"\x1bee",
    },
];

fn sleep(secs: f64) -> Result<(), Stopped> {
    stop::sleep(Duration::from_secs_f64(secs))
}

/// `bytes` as the dump prints them: lowercase hex pairs, space-separated.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The bytes case `n` read, off the screen `text`: the `#n: <hex…>` line, the
/// Enter's trailing `0d` left out. `None` while that line is not on screen.
#[must_use]
pub fn case_bytes(text: &str, n: u32) -> Option<String> {
    let tag = format!("#{n}: ");
    let line = text
        .lines()
        .find_map(|l| l.trim_start().strip_prefix(&tag))?;
    let line = line.trim_end();
    Some(
        line.strip_suffix("0d")
            .unwrap_or(line)
            .trim_end()
            .to_string(),
    )
}

/// What one case came to.
enum Outcome {
    /// The bytes the program read for it.
    Read(String),
    /// Not judged: a person's input arrived during it, or the window did not
    /// hold the keyboard.
    NotMeasured(String),
    /// The case could not run (a refused `hwkey`, no line in time).
    Broken(String),
}

/// How long ago a person last touched a keyboard, mouse or trackpad: the
/// kernel's `HIDIdleTime` (nanoseconds), read with `ioreg`, so nothing here
/// reaches WindowServer. `None` when it cannot be read.
fn hid_idle() -> Option<Duration> {
    let o = aterm_uds::spawnfd::command("/usr/sbin/ioreg")
        .args(["-c", "IOHIDSystem", "-r", "-d", "1", "-k", "HIDIdleTime"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("\"HIDIdleTime\" = "))
        .and_then(|n| n.trim().parse::<u64>().ok())
        .map(Duration::from_nanos)
}

struct Lane {
    inst: Instance,
    /// The next case's number, as the dump counts Enters.
    next: u32,
    oks: u32,
    fails: u32,
    /// Meta rows that read the wrong bytes, and cases that could not run.
    findings: u32,
    /// Control rows that did not compose: the Meta rows are then not measured.
    uncontrolled: u32,
    /// Cases not judged ([`Outcome::NotMeasured`]).
    unmeasured: u32,
}

impl Lane {
    fn ctl(&self, args: &[&str]) -> String {
        self.inst.ctl(args).first()
    }

    fn row(&mut self, ok: bool, what: &str, evidence: &str) {
        if ok {
            self.oks += 1;
        } else {
            self.fails += 1;
        }
        println!(
            "{:<4}  {what:<56}  {evidence}",
            if ok { "ok" } else { "FAIL" }
        );
    }

    /// Press `case`'s keys and Enter; the bytes the program read for them.
    fn press(&mut self, case: &Case) -> Result<Outcome, Stopped> {
        let started = std::time::Instant::now();
        let mut held = true;
        for key in case.keys.iter().chain(&["enter", "enter"]) {
            let mut args = vec!["hwkey"];
            args.extend(key.split_whitespace());
            args.push("shape=press");
            let r = self.ctl(&args);
            if !r.starts_with("OK") {
                return Ok(Outcome::Broken(format!("hwkey {key}: {r}")));
            }
            held &= r.contains("key=1");
            sleep(0.08)?;
        }
        let n = self.next;
        self.next += 1;
        let tag = format!("#{n}:");
        let waited = self.inst.ctl(&["await", "match", &tag, "timeout=60000"]);
        if waited.code != 0 {
            return Ok(Outcome::Broken(format!(
                "no {tag} line in 60 s: {}",
                waited.first()
            )));
        }
        let text = self.inst.ctl(&["text"]).stdout;
        let Some(read) = case_bytes(&text, n) else {
            return Ok(Outcome::Broken(format!("{tag} left the screen")));
        };
        if hid_idle().is_none_or(|idle| idle < started.elapsed()) {
            return Ok(Outcome::NotMeasured(format!(
                "a person touched the keyboard or pointer during it (read [{read}])"
            )));
        }
        if !held {
            return Ok(Outcome::NotMeasured(format!(
                "hwkey said key=0: the window was not the key window of the active app \
                 (read [{read}])"
            )));
        }
        Ok(Outcome::Read(read))
    }

    fn not_measured(&mut self, what: &str, why: &str) {
        self.unmeasured += 1;
        println!("{:<4}  {what:<56}  NOT MEASURED: {why}", "--");
    }

    fn control(&mut self, when: &str) -> Result<(), Stopped> {
        println!("---- C ({when}): Meta off, the input method composes");
        println!(
            "      settings set option_as_meta false: {}",
            self.ctl(&["settings", "set", "option_as_meta", "false"])
        );
        sleep(0.5)?;
        for case in &CONTROL {
            let what = format!("C: {}", case.what);
            match self.press(case)? {
                Outcome::Read(got) => {
                    let ok = got == hex(case.want);
                    if !ok {
                        self.uncontrolled += 1;
                    }
                    let evidence = format!("read [{got}], want [{}]", hex(case.want));
                    self.row(ok, &what, &evidence);
                }
                Outcome::NotMeasured(why) => {
                    self.uncontrolled += 1;
                    self.not_measured(&what, &why);
                }
                Outcome::Broken(why) => {
                    self.uncontrolled += 1;
                    self.findings += 1;
                    self.row(false, &what, &why);
                }
            }
        }
        Ok(())
    }

    fn meta(&mut self) -> Result<(), Stopped> {
        println!("---- M: Meta on (option_as_meta's default)");
        println!(
            "      settings unset option_as_meta: {}",
            self.ctl(&["settings", "unset", "option_as_meta"])
        );
        sleep(0.5)?;
        for case in &META {
            let what = format!("M: {}", case.what);
            match self.press(case)? {
                Outcome::Read(got) => {
                    let ok = got == hex(case.want);
                    if !ok {
                        self.findings += 1;
                    }
                    let evidence = format!("read [{got}], want [{}]", hex(case.want));
                    self.row(ok, &what, &evidence);
                }
                Outcome::NotMeasured(why) => self.not_measured(&what, &why),
                Outcome::Broken(why) => {
                    self.findings += 1;
                    self.row(false, &what, &why);
                }
            }
        }
        Ok(())
    }
}

/// Whether this is a logged-in GUI session (`launchctl managername` says `Aqua`).
fn gui_session() -> bool {
    aterm_uds::spawnfd::command("/bin/launchctl")
        .arg("managername")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "Aqua")
}

/// Whether every keyboard layout this Mac has enabled is U.S. (`com.apple.HIToolbox`'s
/// `AppleEnabledInputSources`, as `defaults` prints it); `None` when it cannot be read.
fn us_layout() -> Option<bool> {
    let o = aterm_uds::spawnfd::command("/usr/bin/defaults")
        .args(["read", "com.apple.HIToolbox", "AppleEnabledInputSources"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&o.stdout);
    let layouts: Vec<&str> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("\"KeyboardLayout Name\" = "))
        .collect();
    (!layouts.is_empty()).then(|| {
        layouts
            .iter()
            .all(|l| l.trim_end_matches(';') == "\"U.S.\"")
    })
}

/// `meta-keys --window [--aterm PATH] [--keep]`.
///
/// # Errors
/// Usage, a lane that could not run, or an interrupt.
pub fn cmd_meta_keys(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let window = instance::take_flag(&mut a, "--window");
    let keep = instance::take_flag(&mut a, "--keep");
    let aterm = instance::take_value(&mut a, "--aterm")?;
    if !window {
        return Err(Fail::usage(format!(
            "meta-keys opens a window on your desktop and presses keys in it: pass --window to \
             say you want that ({LAB} help)"
        )));
    }
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "meta-keys: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let not_run = |why: String| Fail::exit(3, format!("NOT RUN: {why}"));
    let aterm = instance::resolve_aterm(aterm).map_err(not_run)?;
    if !gui_session() {
        return Err(not_run(
            "no GUI session (launchctl managername is not Aqua): the lane opens a window".into(),
        ));
    }
    if us_layout() != Some(true) {
        return Err(not_run(
            "the rows read the U.S. layout's characters, and this Mac has another keyboard \
             layout enabled"
                .into(),
        ));
    }
    let mut world = World::new(CONFIG_OFF)?;
    if keep {
        world.keep();
    }
    let exe = crate::paths::paths().exe.to_string_lossy().into_owned();
    let inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.clone(),
            headless: false,
            args: ["--columns", "80", "--lines", "24", "-e", &exe, "key-dump"]
                .map(str::to_string)
                .to_vec(),
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
        },
    )?;
    println!(
        "== {} ; windowed instance pid={} sock={}",
        aterm.display(),
        inst.pid,
        inst.sock.display()
    );
    let mut lane = Lane {
        inst,
        next: 1,
        oks: 0,
        fails: 0,
        findings: 0,
        uncontrolled: 0,
        unmeasured: 0,
    };
    if let Err(f) = lane.inst.await_socket(instance::BOOT) {
        lane.row(
            false,
            "boot: the instance answers its socket",
            &f.msg.unwrap_or_default(),
        );
        return Ok(1);
    }
    if lane
        .inst
        .ctl(&["await", "match", READY, "timeout=60000"])
        .code
        != 0
    {
        lane.row(false, "boot: the key dump is up", "no ready line in 60 s");
        return Ok(1);
    }
    let sid = lane
        .inst
        .ctl(&["sessions"])
        .stdout
        .split_whitespace()
        .find(|w| {
            w.len() == 22 && w.starts_with("s-") && w[2..].chars().all(|c| c.is_ascii_hexdigit())
        })
        .map(str::to_string);
    let Some(sid) = sid else {
        lane.row(
            false,
            "boot: the tab has a session id",
            "none in `sessions`",
        );
        return Ok(1);
    };
    // Take the keyboard only from an idle desk.
    let asked = std::time::Instant::now();
    loop {
        match hid_idle() {
            Some(idle) if idle >= IDLE => break,
            Some(_) if asked.elapsed() < IDLE_WAIT => sleep(0.5)?,
            Some(_) => {
                return Err(not_run(format!(
                    "a person used the keyboard or pointer all through {} s; the lane takes \
                     the keyboard only from an idle desk",
                    IDLE_WAIT.as_secs()
                )));
            }
            None => {
                return Err(not_run(
                    "the HID idle clock (ioreg HIDIdleTime) is unreadable".into(),
                ));
            }
        }
    }
    println!("      raise {sid}: {}", lane.ctl(&["raise", &sid]));
    sleep(1.0)?;
    lane.control("before")?;
    lane.meta()?;
    lane.control("after")?;
    let _ = lane.inst.ctl(&["send", "q"]);
    println!(
        "\n{} ok, {} FAIL, {} not measured{}",
        lane.oks,
        lane.fails,
        lane.unmeasured,
        if lane.uncontrolled > 0 {
            " — the control did not compose, so Meta's \"no composition\" was not measured"
        } else {
            ""
        }
    );
    // A control row that did not compose says the Meta rows were not measured;
    // only a Meta row's wrong bytes, or a case that could not run, is a finding.
    Ok(if lane.findings > 0 {
        1
    } else if lane.uncontrolled > 0 || lane.unmeasured > 0 {
        3
    } else {
        0
    })
}

// ------------------------------------------------------------------ the dump

/// The dump's `main` (`aterm-lab key-dump`, the tab's program): raw mode, then one
/// line per Enter, `#<n>: <hex of every byte read since the last Enter, the Enter
/// included>`, a lone Enter dropped; end of file or `q` ends it.
#[must_use]
pub fn keydump_main(_args: &[String]) -> i32 {
    let _raw = crate::sys::RawMode::enter(0);
    let put = |s: &str| {
        let _ = crate::sys::write_all(1, s.as_bytes());
    };
    put(&format!("{READY}\r\n"));
    let mut n = 1u32;
    let mut held: Vec<u8> = Vec::new();
    loop {
        let mut b = [0u8; 64];
        let got = crate::sys::read(0, &mut b).unwrap_or(0);
        if got == 0 {
            return 0;
        }
        for &byte in &b[..got] {
            if byte == b'q' && held.is_empty() {
                return 0;
            }
            if byte == b'\r' && held.is_empty() {
                continue;
            }
            held.push(byte);
            if byte == b'\r' {
                put(&format!("#{n}: {}\r\n", hex(&held)));
                held.clear();
                n += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_case_reads_its_own_line_without_the_enter() {
        let screen = "key-dump ready\n#1: c3 a9 0d\n#2: 1b 62 0d\n#12: 1b 65 0d\n";
        assert_eq!(case_bytes(screen, 1).as_deref(), Some("c3 a9"));
        assert_eq!(case_bytes(screen, 2).as_deref(), Some("1b 62"));
        assert_eq!(case_bytes(screen, 12).as_deref(), Some("1b 65"));
        assert_eq!(case_bytes(screen, 3), None);
    }

    #[test]
    fn the_wanted_bytes_print_as_the_dump_prints_them() {
        assert_eq!(hex(b"\x1bb"), "1b 62");
        assert_eq!(hex("∫".as_bytes()), "e2 88 ab");
        assert_eq!(hex("é".as_bytes()), "c3 a9");
    }

    #[test]
    fn meta_never_wants_a_composed_byte_and_the_control_always_does() {
        for case in &META {
            assert_eq!(case.want[0], 0x1b, "{}", case.what);
            assert!(case.want.is_ascii(), "{}", case.what);
        }
        for case in &CONTROL {
            assert!(!case.want.is_ascii(), "{}", case.what);
        }
    }
}
