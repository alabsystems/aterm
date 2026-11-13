// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `meta-keys --window`.

use std::time::{Duration, Instant};

use crate::input_sources::{self, HiToolbox, Sources};
use crate::instance::{self, CONFIG_OFF, Instance, Launch, World};
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

/// The ready line the dump prints once its terminal is raw.
const READY: &str = "key-dump ready";

/// How long no person may have touched the keyboard or pointer before the lane
/// takes the keyboard or switches a layout.
const IDLE: Duration = Duration::from_secs(3);

/// How long the lane waits for that quiet: a hang detector, never a budget.
const IDLE_WAIT: Duration = Duration::from_mins(5);

/// The layouts a run measures unless `--layout` names others.
const LAYOUTS: [&str; 3] = [
    "com.apple.keylayout.US",
    "com.apple.keylayout.German",
    "com.apple.keylayout.French",
];

/// The layout whose characters name the keys `hwkey` presses.
const US: &str = "com.apple.keylayout.US";

/// UCKeyTranslate's modifier byte (Carbon's `EventModifiers` >> 8).
pub const SHIFT: u32 = 0x02;
pub const OPTION: u32 = 0x08;

/// The keys a row may press: the main block's character keys, Return (36), Tab
/// (48) and Space (49) aside.
fn key_codes() -> impl Iterator<Item = u16> {
    (0..=50u16).filter(|c| !matches!(c, 36 | 48 | 49))
}

/// The space bar, pressed after a dead key to read its accent.
const SPACE: u16 = 49;

/// What a layout's key types ([`crate::sys::tis::Layout`], or a test's table).
pub trait Keys {
    /// The text `key_code` types with `mods` ([`SHIFT`], [`OPTION`]) after the
    /// dead-key state `dead`, which it updates; `None` when the layout refuses.
    fn press(&self, key_code: u16, mods: u32, dead: &mut u32) -> Option<String>;
}

impl Keys for crate::sys::tis::Layout {
    fn press(&self, key_code: u16, mods: u32, dead: &mut u32) -> Option<String> {
        crate::sys::tis::Layout::press(self, key_code, mods, dead)
    }
}

/// What one press types, with no dead key before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Typed {
    /// One printable character.
    Char(char),
    /// A dead key, and the state it leaves.
    Dead(u32),
    /// Nothing printable (a control code, several characters, a refusal).
    Other,
}

fn typed(k: &impl Keys, code: u16, mods: u32) -> Typed {
    let mut dead = 0;
    let Some(s) = k.press(code, mods, &mut dead) else {
        return Typed::Other;
    };
    if s.is_empty() && dead != 0 {
        return Typed::Dead(dead);
    }
    one_char(&s).map_or(Typed::Other, Typed::Char)
}

/// `s` when it is one printable character.
fn one_char(s: &str) -> Option<char> {
    let mut cs = s.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) if !c.is_control() && !c.is_whitespace() => Some(c),
        _ => None,
    }
}

/// What `code`, bare, types after the dead-key state `dead`.
fn after_dead(k: &impl Keys, dead: u32, code: u16) -> Option<String> {
    let mut d = dead;
    k.press(code, 0, &mut d)
}

/// One case: the `hwkey` presses before its Enter, and the bytes the program must read for them (the Enter's `0d` left out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub what: String,
    pub keys: Vec<String>,
    pub want: Vec<u8>,
}

/// A layout's rows.
#[derive(Debug, Default)]
pub struct Rows {
    pub control: Vec<Case>,
    pub meta: Vec<Case>,
}

fn utf8(c: char) -> Vec<u8> {
    c.to_string().into_bytes()
}

/// ESC and `text`.
fn esc(text: &str) -> Vec<u8> {
    let mut v = vec![0x1b];
    v.extend_from_slice(text.as_bytes());
    v
}

/// Whether `s` is `c`'s capital, which Shift types on a letter key on any layout.
fn capital_of(c: char, s: char) -> bool {
    let mut up = c.to_uppercase();
    up.next() == Some(s) && up.next().is_none()
}

/// Add a Meta row, unless an earlier one presses the same keys.
fn push_meta(rows: &mut Rows, case: Case) {
    if !rows.meta.iter().any(|m| m.keys == case.keys) {
        rows.meta.push(case);
    }
}

/// The rows of `layout`, derived from it.
#[must_use]
pub fn rows(layout: &impl Keys, us: &impl Keys) -> Rows {
    let keys: Vec<(u16, char, Typed)> = key_codes()
        .filter_map(|code| match typed(us, code, 0) {
            Typed::Char(n) if n.is_ascii_graphic() => Some((code, n, typed(layout, code, 0))),
            _ => None,
        })
        .collect();
    let accent = |dead: u32| after_dead(layout, dead, SPACE).unwrap_or_default();
    let puts = |code: u16, mods: u32| match typed(layout, code, mods) {
        Typed::Char(c) => Some(c),
        Typed::Dead(d) => one_char(&accent(d)),
        Typed::Other => None,
    };
    let reach: std::collections::HashSet<char> = (0..128u16)
        .flat_map(|code| [puts(code, 0), puts(code, SHIFT)])
        .flatten()
        .collect();
    let only = |code: u16, mods: u32| {
        puts(code, mods).filter(|c| c.is_ascii_graphic() && !reach.contains(c))
    };
    let base = |dead: u32| {
        keys.iter().find_map(|&(code, n, bare)| {
            let Typed::Char(b) = bare else { return None };
            let x = one_char(&after_dead(layout, dead, code)?)?;
            (!x.is_ascii() && x != b).then_some((n, b, x))
        })
    };
    let mut rows = Rows::default();

    let mut composed = None;
    for &(code, n, bare) in &keys {
        let (Typed::Dead(d), Typed::Char(b)) = (typed(layout, code, OPTION), bare) else {
            continue;
        };
        let a = accent(d);
        let alone = only(code, OPTION);
        push_meta(
            &mut rows,
            match alone {
                Some(a) => Case {
                    what: format!("Option+{b} (dead {a}) types {a}: only Option reaches it"),
                    keys: vec![format!("{n} mods=alt")],
                    want: utf8(a),
                },
                None => Case {
                    what: format!("Option+{b} (dead {a}) goes out as ESC {b}"),
                    keys: vec![format!("{n} mods=alt")],
                    want: esc(&b.to_string()),
                },
            },
        );
        if composed.is_none() {
            composed = base(d).map(|base| (n, b, a, alone, base));
        }
    }
    if let Some((n, b, a, alone, (bn, bc, x))) = composed {
        let keys = vec![format!("{n} mods=alt"), bn.to_string()];
        rows.control.push(Case {
            what: format!("Option+{b} (dead {a}) then {bc} composes {x}"),
            keys: keys.clone(),
            want: utf8(x),
        });
        push_meta(
            &mut rows,
            match alone {
                Some(a) => Case {
                    what: format!(
                        "Option+{b} then {bc} is {a} {bc}: only Option reaches {a}, no dead key \
                         pending"
                    ),
                    keys,
                    want: format!("{a}{bc}").into_bytes(),
                },
                None => Case {
                    what: format!("Option+{b} then {bc} is ESC {b} {bc}: no dead key pending"),
                    keys,
                    want: esc(&format!("{b}{bc}")),
                },
            },
        );
    }

    for (ascii, alone) in [(false, false), (true, true), (true, false)] {
        let chord =
            keys.iter().find_map(
                |&(code, n, bare)| match (typed(layout, code, OPTION), bare) {
                    (Typed::Char(o), Typed::Char(b))
                        if o != b
                            && o.is_ascii() == ascii
                            && only(code, OPTION).is_some() == alone =>
                    {
                        Some((n, b, o))
                    }
                    _ => None,
                },
            );
        if let Some((n, b, o)) = chord {
            let keys = vec![format!("{n} mods=alt")];
            rows.control.push(Case {
                what: format!("Option+{b} types {o}"),
                keys: keys.clone(),
                want: utf8(o),
            });
            push_meta(
                &mut rows,
                if alone {
                    Case {
                        what: format!("Option+{b} types {o}: only Option reaches it"),
                        keys,
                        want: utf8(o),
                    }
                } else {
                    Case {
                        what: format!("Option+{b} goes out as ESC {b}, not {o}"),
                        keys,
                        want: esc(&b.to_string()),
                    }
                },
            );
        }
    }

    let shifted: Vec<(char, char, char, bool)> = keys
        .iter()
        .filter_map(
            |&(code, n, bare)| match (bare, typed(layout, code, SHIFT)) {
                (Typed::Char(b), Typed::Char(s))
                    if !capital_of(b, s) && only(code, OPTION | SHIFT).is_none() =>
                {
                    Some((n, b, s, typed(us, code, SHIFT) != Typed::Char(s)))
                }
                _ => None,
            },
        )
        .collect();
    if let Some(&(n, b, s, _)) = shifted.iter().find(|r| r.3).or(shifted.first()) {
        push_meta(
            &mut rows,
            Case {
                what: format!("Option+Shift+{b} goes out as ESC {s}"),
                keys: vec![format!("{n} mods=alt+shift")],
                want: esc(&s.to_string()),
            },
        );
    }

    let foreign = keys.iter().find_map(|&(code, n, bare)| match bare {
        Typed::Char(b) if !b.is_ascii() && only(code, OPTION).is_none() => Some((n, b)),
        _ => None,
    });
    if let Some((n, b)) = foreign {
        push_meta(
            &mut rows,
            Case {
                what: format!("Option+{b} goes out as ESC {b}"),
                keys: vec![format!("{n} mods=alt")],
                want: esc(&b.to_string()),
            },
        );
    }

    let plain = keys.iter().find_map(|&(_, n, bare)| match bare {
        Typed::Dead(d) => base(d).map(|base| (n, accent(d), base)),
        _ => None,
    });
    if let Some((n, a, (bn, bc, x))) = plain {
        push_meta(
            &mut rows,
            Case {
                what: format!("{a} (a dead key, no Option) then {bc} composes {x}"),
                keys: vec![n.to_string(), bn.to_string()],
                want: utf8(x),
            },
        );
    }
    rows
}

fn sleep(secs: f64) -> Result<(), Stopped> {
    stop::sleep(Duration::try_from_secs_f64(secs).unwrap_or(Duration::ZERO))
}

/// `bytes` as the dump prints them.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The bytes case `n` read, off the screen `text`.
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

/// How long ago a person last touched a keyboard, mouse or trackpad.
pub(crate) fn hid_idle() -> Option<Duration> {
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

/// Wait until no person has touched the keyboard or pointer for [`IDLE`].
///
/// # Errors
/// `Ok(Err(why))`: the desk was never idle in [`IDLE_WAIT`], or the idle clock is
/// unreadable.
pub(crate) fn await_idle() -> Result<Result<(), String>, Stopped> {
    let asked = Instant::now();
    loop {
        match hid_idle() {
            Some(idle) if idle >= IDLE => return Ok(Ok(())),
            Some(_) if asked.elapsed() < IDLE_WAIT => sleep(0.5)?,
            Some(_) => {
                return Ok(Err(format!(
                    "a person used the keyboard or pointer all through {} s; the lane takes \
                     the keyboard and switches a layout only at an idle desk",
                    IDLE_WAIT.as_secs()
                )));
            }
            None => {
                return Ok(Err(
                    "the HID idle clock (ioreg HIDIdleTime) is unreadable".into()
                ));
            }
        }
    }
}

struct Lane {
    inst: Instance,
    /// The next case's number, as the dump counts Enters.
    next: u32,
    oks: u32,
    fails: u32,
    /// Meta rows that read the wrong bytes, cases that could not run, and input
    /// sources that did not come back.
    findings: u32,
    /// Control rows that did not compose: the Meta rows are then not measured.
    uncontrolled: u32,
    /// Cases not judged ([`Outcome::NotMeasured`]), and layouts not run.
    unmeasured: u32,
    /// A person touched the keyboard or pointer during a case.
    person: bool,
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
            "{:<4}  {what:<64}  {evidence}",
            if ok { "ok" } else { "FAIL" }
        );
    }

    /// Press `case`'s keys and Enter; the bytes the program read for them.
    fn press(&mut self, case: &Case) -> Result<Outcome, Stopped> {
        let started = Instant::now();
        let mut held = true;
        for key in case
            .keys
            .iter()
            .map(String::as_str)
            .chain(["enter", "enter"])
        {
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
            self.person = true;
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
        println!("{:<4}  {what:<64}  NOT MEASURED: {why}", "--");
    }

    fn control(&mut self, cases: &[Case], when: &str) -> Result<(), Stopped> {
        println!("---- C ({when}): Meta off, the input method composes");
        println!(
            "      settings set option_as_meta false: {}",
            self.ctl(&["settings", "set", "option_as_meta", "false"])
        );
        sleep(0.5)?;
        for case in cases {
            let what = format!("C: {}", case.what);
            match self.press(case)? {
                Outcome::Read(got) => {
                    let ok = got == hex(&case.want);
                    if !ok {
                        self.uncontrolled += 1;
                    }
                    let evidence = format!("read [{got}], want [{}]", hex(&case.want));
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
            if self.person {
                break;
            }
        }
        Ok(())
    }

    fn meta(&mut self, cases: &[Case]) -> Result<(), Stopped> {
        println!("---- M: Meta on (option_as_meta's default)");
        println!(
            "      settings unset option_as_meta: {}",
            self.ctl(&["settings", "unset", "option_as_meta"])
        );
        sleep(0.5)?;
        for case in cases {
            let what = format!("M: {}", case.what);
            match self.press(case)? {
                Outcome::Read(got) => {
                    let ok = got == hex(&case.want);
                    if !ok {
                        self.findings += 1;
                    }
                    let evidence = format!("read [{got}], want [{}]", hex(&case.want));
                    self.row(ok, &what, &evidence);
                }
                Outcome::NotMeasured(why) => self.not_measured(&what, &why),
                Outcome::Broken(why) => {
                    self.findings += 1;
                    self.row(false, &what, &why);
                }
            }
            if self.person {
                break;
            }
        }
        Ok(())
    }
}

/// The lane as [`input_sources::in_turn`] runs it, one layout at a time.
struct Turns<'a> {
    lane: &'a mut Lane,
    plans: &'a [(String, Rows)],
    /// The tab's session, raised before each layout's rows.
    sid: &'a str,
}

impl input_sources::Lane for Turns<'_> {
    fn await_idle(&mut self) -> Result<Result<(), String>, Stopped> {
        await_idle()
    }

    fn measure(&mut self, id: &str) -> Result<(), Stopped> {
        let Some((_, plan)) = self.plans.iter().find(|(p, _)| p == id) else {
            return Ok(());
        };
        let lane = &mut *self.lane;
        println!(
            "\n==== {} ({id}) selected: {} C and {} M rows",
            short(id),
            plan.control.len(),
            plan.meta.len()
        );
        println!(
            "      raise {}: {}",
            self.sid,
            lane.ctl(&["raise", self.sid])
        );
        sleep(1.0)?;
        lane.control(&plan.control, "before")?;
        if !lane.person {
            lane.meta(&plan.meta)?;
        }
        if !lane.person {
            lane.control(&plan.control, "after")?;
        }
        Ok(())
    }

    fn person(&self) -> bool {
        self.lane.person
    }

    fn skipped(&mut self, id: &str, why: &str) {
        self.lane
            .not_measured(&format!("layout {}", short(id)), why);
    }

    fn refused(&mut self, id: &str, what: &str, why: &str) {
        self.lane.findings += 1;
        self.lane.row(
            false,
            &format!("layout {}: input sources {what}", short(id)),
            why,
        );
    }
}

/// Whether this is a logged-in GUI session (`launchctl managername` says `Aqua`).
fn gui_session() -> bool {
    aterm_uds::spawnfd::command("/bin/launchctl")
        .arg("managername")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "Aqua")
}

/// `defaults read com.apple.HIToolbox AppleEnabledInputSources`, as it prints it.
fn enabled_defaults() -> String {
    aterm_uds::spawnfd::command("/usr/bin/defaults")
        .args(["read", "com.apple.HIToolbox", "AppleEnabledInputSources"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// The input sources as the lane compares them before and after its run.
#[derive(Debug, PartialEq, Eq)]
struct Desk {
    selected: Option<String>,
    enabled: Vec<String>,
    defaults: String,
    /// HIToolbox's own bookkeeping (`AppleInputSourceHistory` and its kin).
    bookkeeping: Result<Vec<input_sources::Pref>, String>,
}

fn desk() -> Desk {
    Desk {
        selected: HiToolbox.selected(),
        enabled: HiToolbox.enabled(),
        defaults: enabled_defaults(),
        bookkeeping: HiToolbox.bookkeeping(),
    }
}

/// The short name of an input source ID (`German` for `com.apple.keylayout.German`).
fn short(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

/// A second interrupt's last act, on the stop thread: `meta-keys --restore` in a
/// child, on whose main thread HIToolbox answers, waited on for at most a minute
/// (a hang detector).
fn restore_in_child() {
    let exe = &crate::paths::paths().exe;
    let Ok(mut child) = aterm_uds::spawnfd::command(exe)
        .args(["meta-keys", "--restore"])
        .spawn()
    else {
        return;
    };
    let end = Instant::now() + Duration::from_mins(1);
    while Instant::now() < end {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
}

/// `meta-keys --restore`: put back what a run's record names, then print the
/// input sources as HIToolbox has them now.
fn cmd_restore() -> Res {
    let path = input_sources::record_path();
    match input_sources::restore_now(&HiToolbox, &path) {
        Ok(Some(r)) => println!(
            "restored from {}: selected {} again{}",
            path.display(),
            r.selected,
            if r.enabled.is_empty() {
                String::new()
            } else {
                format!(", disabled {}", r.enabled.join(", "))
            }
        ),
        Ok(None) => println!("no record at {}: nothing to put back", path.display()),
        Err(e) => return Err(Fail::exit(1, format!("meta-keys --restore: {e}"))),
    }
    let now = desk();
    println!(
        "input sources: selected {}; enabled {}",
        now.selected.as_deref().unwrap_or("(none)"),
        now.enabled.join(", ")
    );
    Ok(0)
}

/// Plan one composition on the currently selected source without changing input sources.
fn current_control() -> Res {
    let selected = crate::sys::tis::selected();
    let read = || -> Result<aterm_json::Value, String> {
        let id = selected
            .as_deref()
            .ok_or("HIToolbox names no selected keyboard input source")?;
        let layout = crate::sys::tis::Layout::of(id)?;
        let us = crate::sys::tis::Layout::of(US)?;
        let plan = rows(&layout, &us);
        let case = plan
            .control
            .first()
            .ok_or("the selected layout has no control row")?;
        let want = std::str::from_utf8(&case.want).map_err(|e| e.to_string())?;
        if case.keys.len() < 2 || want.is_ascii() || want.chars().count() != 1 {
            return Err("the selected layout's first control is not a multikey composition".into());
        }
        if crate::sys::tis::selected().as_deref() != Some(id) {
            return Err("the selected input source changed while its control was read".into());
        }
        Ok(aterm_json::json!({
            "status": "READY",
            "selected_id": id,
            "keyboard_type": layout.keyboard_type(),
            "what": case.what.clone(),
            "keys": case.keys.clone(),
            "want_utf8": want,
            "want_hex": hex(&case.want),
        }))
    };
    let (report, code) = match read() {
        Ok(report) => (report, 0),
        Err(reason) => (
            aterm_json::json!({
                "status": "UNOBSERVED",
                "selected_id": selected,
                "reason": reason,
            }),
            3,
        ),
    };
    println!(
        "{}",
        aterm_json::to_string(&report).map_err(|e| Fail::die(e.to_string()))?
    );
    Ok(code)
}

/// `meta-keys --window [--layout ID]... [--aterm PATH] [--keep]`, `--current-control`, or `--restore`.
///
/// # Errors
/// Usage, a lane that could not run, or an interrupt.
pub fn cmd_meta_keys(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let current = instance::take_flag(&mut a, "--current-control");
    let window = instance::take_flag(&mut a, "--window");
    let keep = instance::take_flag(&mut a, "--keep");
    let restore = instance::take_flag(&mut a, "--restore");
    let plan_only = instance::take_flag(&mut a, "--plan");
    let aterm = instance::take_value(&mut a, "--aterm")?;
    let asked = instance::take_values(&mut a, "--layout")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "meta-keys: unexpected '{extra}' ({LAB} help)"
        )));
    }
    if current {
        if window || keep || restore || plan_only || aterm.is_some() || !asked.is_empty() {
            return Err(Fail::usage(
                "meta-keys --current-control takes no other options",
            ));
        }
        return current_control();
    }
    if restore {
        return cmd_restore();
    }
    if !window && !plan_only {
        return Err(Fail::usage(format!(
            "meta-keys opens a window on your desktop, selects each keyboard layout it \
             measures and presses keys in it: pass --window to say you want that ({LAB} help)"
        )));
    }
    let layouts: Vec<String> = if asked.is_empty() {
        LAYOUTS.iter().map(|s| (*s).to_string()).collect()
    } else {
        asked
            .iter()
            .map(|l| {
                if l.contains('.') {
                    l.clone()
                } else {
                    format!("com.apple.keylayout.{l}")
                }
            })
            .collect()
    };
    let not_run = |why: String| Fail::exit(3, format!("NOT RUN: {why}"));
    let aterm = instance::resolve_aterm(aterm).map_err(not_run)?;
    if !gui_session() {
        return Err(not_run(
            "no GUI session (launchctl managername is not Aqua): the lane opens a window".into(),
        ));
    }
    let us = crate::sys::tis::Layout::of(US).map_err(not_run)?;
    let mut plans = Vec::new();
    for id in &layouts {
        let layout = crate::sys::tis::Layout::of(id).map_err(not_run)?;
        plans.push((id.clone(), rows(&layout, &us)));
    }
    println!("== keyboard type {} (LMGetKbdType)", us.keyboard_type());
    if plan_only {
        for (id, plan) in &plans {
            println!("\n==== {} ({id})", short(id));
            for (tag, cases) in [("C", &plan.control), ("M", &plan.meta)] {
                for case in cases {
                    println!(
                        "{tag}: {:<60}  press [{}], want [{}]",
                        case.what,
                        case.keys.join(" | "),
                        hex(&case.want)
                    );
                }
            }
        }
        return Ok(0);
    }
    let record = input_sources::record_path();
    match input_sources::recover(&HiToolbox, &record) {
        Ok(Some(said)) => println!("== {said}"),
        Ok(None) => {}
        Err(e) => return Err(not_run(e)),
    }
    let before = desk();
    println!(
        "== before: selected {}; enabled {}",
        before.selected.as_deref().unwrap_or("(none)"),
        before.enabled.join(", ")
    );
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
            reroute: false,
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
        person: false,
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
    stop::at_hard_exit(restore_in_child);
    let ids: Vec<String> = plans.iter().map(|(id, _)| id.clone()).collect();
    input_sources::in_turn(
        &HiToolbox,
        &record,
        &ids,
        &mut Turns {
            lane: &mut lane,
            plans: &plans,
            sid: &sid,
        },
    )?;
    let _ = lane.inst.ctl(&["send", "q"]);
    let after = desk();
    println!(
        "\n== after: selected {}; enabled {}",
        after.selected.as_deref().unwrap_or("(none)"),
        after.enabled.join(", ")
    );
    let ok = after.selected == before.selected;
    if !ok {
        lane.findings += 1;
    }
    lane.row(
        ok,
        "restore: the selected input source is the one before",
        after.selected.as_deref().unwrap_or("(none)"),
    );
    let ok = after.enabled == before.enabled;
    if !ok {
        lane.findings += 1;
    }
    lane.row(
        ok,
        "restore: the enabled input sources are the ones before",
        &after.enabled.join(", "),
    );
    let ok = after.defaults == before.defaults;
    if !ok {
        lane.findings += 1;
        println!(
            "      before:\n{}      after:\n{}",
            before.defaults, after.defaults
        );
    }
    lane.row(
        ok,
        "restore: defaults' AppleEnabledInputSources is as before",
        &format!("{} lines", after.defaults.lines().count()),
    );
    let ok = after.bookkeeping.is_ok() && after.bookkeeping == before.bookkeeping;
    if !ok {
        lane.findings += 1;
    }
    lane.row(
        ok,
        "restore: HIToolbox's own bookkeeping (the history) is as before",
        &match &after.bookkeeping {
            Ok(prefs) => prefs
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{k} {}",
                        v.as_ref()
                            .map_or("absent".into(), |v| format!("{} bytes", v.len()))
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
            Err(e) => e.clone(),
        },
    );
    println!(
        "\n{} ok, {} FAIL, {} not measured{}",
        lane.oks,
        lane.fails,
        lane.unmeasured,
        if lane.uncontrolled > 0 {
            " — a control did not compose, so Meta's \"no composition\" was not measured there"
        } else {
            ""
        }
    );
    Ok(if lane.findings > 0 {
        1
    } else if lane.uncontrolled > 0 || lane.unmeasured > 0 {
        3
    } else {
        0
    })
}

/// The dump's `main` (`aterm-lab key-dump`, the tab's program).
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
#[path = "meta_keys_tests.rs"]
mod tests;
