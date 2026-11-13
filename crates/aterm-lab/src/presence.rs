// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `presence-flap --window`.

use std::path::PathBuf;
use std::time::Duration;

use crate::instance::{self, CONFIG_OFF, Instance, Launch, Reply, World};
use crate::screen::field;
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

/// The steps, in the order they run.
pub const PHASES: [char; 5] = ['A', 'H', 'B', 'D', 'T'];

fn sleep(secs: f64) -> Result<(), Stopped> {
    stop::sleep(Duration::try_from_secs_f64(secs).unwrap_or(Duration::ZERO))
}

/// The id of a `resize <id> …` ledger row.
#[must_use]
pub fn parse_resize(line: &str) -> Option<u64> {
    line.strip_prefix("resize ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The phases `--phases` names (`"A H"`, `"A,H"` or `"AH"`).
///
/// # Errors
/// A phase that is not one of [`PHASES`].
pub fn parse_phases(spec: &str) -> Result<Vec<char>, String> {
    let mut out = Vec::new();
    for c in spec.chars().filter(|c| !c.is_whitespace() && *c != ',') {
        let c = c.to_ascii_uppercase();
        if !PHASES.contains(&c) {
            return Err(format!("unknown phase: {c} (A, H, B, D, T)"));
        }
        if !out.contains(&c) {
            out.push(c);
        }
    }
    Ok(out)
}

/// The verdict line of a client run: the first line it wrote to stderr (a
/// `turn` verdict, an `ERR`), else to stdout.
fn verdict(r: &Reply) -> String {
    let head = |s: &str| s.lines().next().unwrap_or("").to_string();
    let line = head(&r.stderr);
    let line = if line.is_empty() {
        head(&r.stdout)
    } else {
        line
    };
    line.strip_prefix("aterm-ctl: ")
        .unwrap_or(&line)
        .to_string()
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A reply that gave no reading, as a row's evidence: what was asked, its exit
/// status and its words, trimmed to one line.
fn failed(asked: &str, r: &Reply) -> String {
    let said = |s: &str| {
        let words: Vec<&str> = s
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        cut(&words.join(" | "), 160)
    };
    format!(
        "no reading from `{asked}`: exit {} stdout='{}' stderr='{}'",
        r.code,
        said(&r.stdout),
        said(&r.stderr)
    )
}

/// A `resizes` reply's resize row ids.
///
/// # Errors
/// The reply failed or carried no `OK` header: its words ([`failed`]), never an
/// empty ledger.
fn ledger_rows(asked: &str, r: &Reply) -> Result<Vec<u64>, String> {
    if r.code != 0 || r.header().is_empty() {
        return Err(failed(asked, r));
    }
    Ok(r.stdout.lines().filter_map(parse_resize).collect())
}

struct Probe {
    inst: Instance,
    /// The newest resize row id seen.
    last: u64,
    base_flaps: String,
    base_risks: String,
    oks: u32,
    fails: u32,
}

impl Probe {
    fn ctl(&self, args: &[&str]) -> Reply {
        self.inst.ctl(args)
    }

    fn row(&mut self, ok: bool, what: &str, evidence: &str) {
        if ok {
            self.oks += 1;
        } else {
            self.fails += 1;
        }
        println!(
            "{:<4}  {what:<60}  {evidence}",
            if ok { "ok" } else { "FAIL" }
        );
    }

    fn status(&self) -> String {
        self.ctl(&["status"]).first()
    }

    /// `resizes since=<since>` (every retained row at 0): the reply and its
    /// resize row ids.
    ///
    /// # Errors
    /// The reply gave no reading ([`ledger_rows`]).
    fn resizes(&self, since: u64) -> Result<(Reply, Vec<u64>), String> {
        let since = format!("since={since}");
        let r = self.ctl(&["resizes", &since]);
        let rows = ledger_rows(&format!("resizes {since}"), &r)?;
        Ok((r, rows))
    }

    /// `resizes since=<last>`: its header and how many resize rows are new;
    /// `last` moves past them.
    ///
    /// # Errors
    /// The reply gave no reading ([`ledger_rows`]). `last` stays where it was,
    /// so the next read that succeeds counts the rows this one missed too: a
    /// failed read before a count can only add rows to it.
    fn read_resizes(&mut self, show: bool) -> Result<(String, usize), String> {
        let (r, rows) = self.resizes(self.last)?;
        if let Some(&m) = rows.iter().max() {
            self.last = self.last.max(m);
        }
        if show {
            for l in r
                .stdout
                .lines()
                .filter(|l| l.starts_with("resize ") || l.starts_with("run "))
            {
                println!("      {}", cut(l, 200));
            }
        }
        Ok((r.header(), rows.len()))
    }

    /// Back to `level=quiet`: a human keypress through the OS event queue
    /// folds a foreign turn's story; the fold's own re-grid is read off and the
    /// typing momentum waited out, so a `yield=0.2` does not park.
    fn quiet(&mut self) -> Result<(), Stopped> {
        let level = field(&self.status(), "level").unwrap_or_default();
        if level != "quiet" {
            println!(
                "      (level={level}: hwkey x: {})",
                verdict(&self.ctl(&["hwkey", "x"]))
            );
            sleep(1.0)?;
            let _ = self.read_resizes(false);
            sleep(5.0)?;
        }
        Ok(())
    }

    /// A step's own ledger rows add no flap and no risk.
    fn no_new_flap(&mut self, what: &str) {
        let st = self.status();
        let what = format!("{what}: no flap, no render risk");
        let head = match self.read_resizes(false) {
            Ok((head, _)) => head,
            Err(why) => {
                self.row(false, &what, &why);
                return;
            }
        };
        let flaps = field(&head, "flaps").unwrap_or_else(|| "-".into());
        let risks = field(&head, "risks").unwrap_or_else(|| "-".into());
        let render = field(&st, "render").unwrap_or_default();
        let ok = flaps == self.base_flaps && risks == self.base_risks && render == "ok";
        let evidence = if ok {
            format!("flaps={flaps} risks={risks} render=ok")
        } else {
            format!(
                "flaps={flaps} (was {}) risks={risks} (was {}) render={render}",
                self.base_flaps, self.base_risks
            )
        };
        self.row(ok, &what, &evidence);
    }

    /// `status gen=`: the generation a fence names.
    ///
    /// # Errors
    /// The `status` reply failed or named no generation: its words ([`failed`]).
    fn gen_of_status(&self) -> Result<String, String> {
        let r = self.ctl(&["status"]);
        match field(&r.first(), "gen") {
            Some(g) if r.code == 0 => Ok(g),
            _ => Err(failed("status", &r)),
        }
    }

    fn phase_a(&mut self) -> Result<(), Stopped> {
        println!("---- A: 5 fence-refused turns (stale generation)");
        self.quiet()?;
        let _ = self.read_resizes(false);
        let (mut refused, mut rows) = (0, 0);
        // Why a trial's read failed, until a later read counts the rows it missed.
        let mut unread = None;
        for i in 1..=5 {
            // Unread, the fence is a bare `if-gen=`, which the turn refuses as
            // usage, never `reason=changed`: the trial is not counted refused.
            let g0 = self.gen_of_status().unwrap_or_default();
            let sent = self.ctl(&["send", "."]).first();
            if let Some(seq) = field(&sent, "seq").and_then(|s| s.parse::<u64>().ok()) {
                let next = (seq + 1).to_string();
                let _ = self.ctl(&["await", "seq", &next, "timeout=5000"]);
            }
            let fence = format!("if-gen={g0}");
            let r = verdict(&self.ctl(&["turn", &fence, "yield=0.2", "hello"]));
            sleep(0.8)?;
            match self.read_resizes(true) {
                Ok((_, n)) => {
                    rows += n;
                    // `last` stayed behind a failed read: this one counted its rows.
                    unread = None;
                }
                Err(why) => unread = Some(format!("A{i}: {why}")),
            }
            println!("      A{i} gen0={g0} -> {}", cut(&r, 120));
            if r.contains("skipped reason=changed") {
                refused += 1;
            }
        }
        self.row(
            refused == 5,
            "A: every stale-fenced turn refused reason=changed",
            &format!("{refused} of 5"),
        );
        let (ok, evidence) = match unread {
            None => (rows == 0, format!("{rows} resize rows")),
            Some(why) => (false, format!("{rows} resize rows, then {why}")),
        };
        self.row(ok, "A: a refused turn re-grids nothing", &evidence);
        Ok(())
    }

    fn phase_h(&mut self) -> Result<(), Stopped> {
        println!(
            "---- H: 6 harness-shaped turns from level=quiet (if-gen=<current> yield=0.2 idle=1)"
        );
        let mut typed = 0;
        for i in 1..=6 {
            self.quiet()?;
            // Unread, the fence is a bare `if-gen=`, which the turn refuses as
            // usage: the trial is not counted typed.
            let g = self.gen_of_status().unwrap_or_default();
            let fence = format!("if-gen={g}");
            let r =
                verdict(&self.ctl(&["turn", &fence, "yield=0.2", "idle=1", "timeout=10000", "hi"]));
            sleep(0.8)?;
            println!("      H{i} gen={g} -> {}", cut(&r, 120));
            let _ = self.read_resizes(true);
            if r.contains("submitted=1") {
                typed += 1;
            }
        }
        self.row(
            typed == 6,
            "H: every harness-shaped turn typed",
            &format!("{typed} of 6 submitted=1"),
        );
        self.no_new_flap("H");
        Ok(())
    }

    fn phase_b(&mut self) -> Result<(), Stopped> {
        println!("---- B: a short successful turn (no fence)");
        self.quiet()?;
        let r = verdict(&self.ctl(&["turn", "idle=1", "timeout=10000", "hi"]));
        sleep(0.8)?;
        println!("      B -> {}", cut(&r, 120));
        let _ = self.read_resizes(true);
        let ok = r.contains("submitted=1");
        let evidence = if ok {
            "submitted=1".to_string()
        } else {
            cut(&r, 120)
        };
        self.row(ok, "B: the short turn typed", &evidence);
        self.no_new_flap("B");
        Ok(())
    }

    fn phase_d(&mut self) -> Result<(), Stopped> {
        println!("---- D: a 2 s foreign drive lease");
        self.quiet()?;
        let _ = self.read_resizes(false);
        let acquired = self.ctl(&["lease", "acquire", "holder=probe", "ttl=3000"]);
        println!("      acquire: {}", verdict(&acquired));
        sleep(2.0)?;
        let released = self.ctl(&["lease", "release", "holder=probe"]);
        println!("      release: {}", verdict(&released));
        sleep(0.9)?;
        let (ok, evidence): (bool, String) = match self.read_resizes(true) {
            Ok((_, 0)) => (true, "0 resize rows across acquire, 2 s, release".into()),
            Ok((_, new)) => (false, format!("{new} resize rows")),
            Err(why) => (false, why),
        };
        self.row(ok, "D: a foreign lease re-grids nothing", &evidence);
        Ok(())
    }

    fn phase_t(&mut self, trials: u32) -> Result<(), Stopped> {
        println!(
            "---- T: {trials} back-to-back trials: turn one; status gen=; turn if-gen=<that> two"
        );
        let mut changed = 0;
        // Trials that measured nothing: turn one did not type, `status` named no
        // generation, or turn two neither typed nor was refused.
        let mut unmeasured = Vec::new();
        for i in 1..=trials {
            self.quiet()?;
            let r1 = verdict(&self.ctl(&["turn", "idle=1", "timeout=10000", "one"]));
            let g = match self.gen_of_status() {
                Ok(g) => g,
                Err(why) => {
                    println!("      T{i} one -> {} | {why}", cut(&r1, 60));
                    unmeasured.push(format!("T{i}: {why}"));
                    continue;
                }
            };
            let fence = format!("if-gen={g}");
            let r2 = verdict(&self.ctl(&[
                "turn",
                &fence,
                "yield=0.2",
                "idle=1",
                "timeout=10000",
                "two",
            ]));
            sleep(0.6)?;
            println!(
                "      T{i} one -> {} | two if-gen={g} -> {}",
                cut(&r1, 60),
                cut(&r2, 80)
            );
            let _ = self.read_resizes(false);
            if r2.contains("skipped reason=changed") {
                changed += 1;
            } else if !r1.contains("submitted=1") {
                unmeasured.push(format!("T{i} one: {}", cut(&r1, 80)));
            } else if !r2.contains("submitted=1") {
                unmeasured.push(format!("T{i} two: {}", cut(&r2, 80)));
            }
        }
        let evidence = if unmeasured.is_empty() {
            format!("{changed} of {trials} refused reason=changed")
        } else {
            format!(
                "{changed} of {trials} refused reason=changed; {} measured nothing: {}",
                unmeasured.len(),
                unmeasured.join("; ")
            )
        };
        self.row(
            changed == 0 && unmeasured.is_empty(),
            "T: no follow-up refused by the first turn's own re-grid",
            &evidence,
        );
        self.no_new_flap("T");
        Ok(())
    }

    fn end(&mut self, lines: u32) -> Result<(), Stopped> {
        println!("---- END");
        self.quiet()?;
        let st = self.status();
        let render = field(&st, "render").unwrap_or_default();
        let evidence = if render == "ok" {
            format!(
                "resizes={} flaps={}",
                field(&st, "resizes").unwrap_or_default(),
                field(&st, "flaps").unwrap_or_default()
            )
        } else {
            format!("render={render}")
        };
        self.row(render == "ok", "END: status render=ok", &evidence);
        let all = self.ctl(&["resizes"]);
        let head = all.header();
        println!("      {head}");
        for l in all.stdout.lines().filter(|l| l.starts_with("run ")) {
            println!("      {}", cut(l, 200));
        }
        let clean = field(&head, "flaps").as_deref() == Some(self.base_flaps.as_str())
            && field(&head, "risks").as_deref() == Some(self.base_risks.as_str())
            && field(&head, "render").as_deref() == Some("ok");
        let evidence = if clean {
            format!(
                "flaps={} risks={} render=ok",
                self.base_flaps, self.base_risks
            )
        } else if head.is_empty() {
            "no header".to_string()
        } else {
            head.clone()
        };
        self.row(
            clean,
            "END: the ledger booked no flap and no risk",
            &evidence,
        );
        let drift = self.ctl(&["cast", "drift"]);
        let dhead = verdict(&drift);
        println!("      {}", cut(&dhead, 200));
        let faithful = field(&dhead, "verdict").as_deref() == Some("clean")
            && field(&dhead, "fidelity").as_deref() == Some("pass");
        let evidence = if faithful {
            format!(
                "verdict=clean fidelity=pass runs={}",
                field(&dhead, "runs").unwrap_or_default()
            )
        } else {
            cut(&dhead, 160)
        };
        self.row(faithful, "END: cast drift is clean and faithful", &evidence);
        if !faithful {
            for l in drift.stdout.lines().filter(|l| {
                l.starts_with("run ") || l.starts_with("row ") || l.starts_with("cursor ")
            }) {
                println!("      {}", cut(l, 200));
            }
        }
        let text = self.ctl(&["text"]).stdout;
        let rows: Vec<&str> = text.lines().collect();
        let top = rows.first().copied().unwrap_or("");
        let bottom = rows
            .get(usize::try_from(lines).unwrap_or(24) - 1)
            .copied()
            .unwrap_or("");
        let unshifted = top.contains("transcript line 00") && bottom.contains("context");
        let evidence = if unshifted {
            format!(
                "row 0 the first transcript line, row {} the footer",
                lines - 1
            )
        } else {
            format!("row 0='{top}' row {}='{bottom}'", lines - 1)
        };
        self.row(unshifted, "END: the screen is unshifted", &evidence);
        let _ = self.inst.ctl_stdin(&["send", "--stdin"], b"q");
        sleep(0.5)
    }
}

/// Whether this is a logged-in GUI session (`launchctl managername` says
/// `Aqua`); an ssh or launchd background session has no WindowServer.
fn gui_session() -> bool {
    aterm_uds::spawnfd::command("/bin/launchctl")
        .arg("managername")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "Aqua")
}

/// The grid the window starts at.
const COLUMNS: u32 = 80;
const LINES: u32 = 24;

/// `presence-flap --window|--headless [--aterm PATH] [--client PATH] [--phases "A H B D T"] [--trials N] [--keep]`.
///
/// # Errors
/// Usage, a lane that could not run, or an interrupt.
pub fn cmd_presence_flap(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let window = instance::take_flag(&mut a, "--window");
    let headless = instance::take_flag(&mut a, "--headless");
    let named = instance::take_value(&mut a, "--phases")?;
    let trials = match instance::take_value(&mut a, "--trials")? {
        Some(v) => instance::count("--trials", &v)?,
        None => 6,
    };
    let client = instance::take_value(&mut a, "--client")?;
    let keep = instance::take_flag(&mut a, "--keep");
    if window == headless {
        return Err(Fail::usage(format!(
            "presence-flap opens a window on your desktop: pass --window to say you want that, or \
             --headless to check the lane itself on a headless instance ({LAB} help)"
        )));
    }
    let phases = match named {
        Some(spec) => parse_phases(&spec).map_err(Fail::usage)?,
        None => PHASES.to_vec(),
    };
    let aterm = instance::take_value(&mut a, "--aterm")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "presence-flap: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let not_run = |why: String| Fail::exit(3, format!("NOT RUN: {why}"));
    let aterm = instance::resolve_aterm(aterm).map_err(not_run)?;
    if window && !gui_session() {
        return Err(not_run(
            "no GUI session (launchctl managername is not Aqua): the lane opens a window".into(),
        ));
    }
    let client = match client {
        Some(c) => {
            let c = crate::paths::from_caller(&c);
            if !crate::sys::executable(&c) {
                return Err(not_run(format!("no aterm client at {c}")));
            }
            PathBuf::from(c)
        }
        None => aterm.clone(),
    };
    let mut world = World::new(CONFIG_OFF)?;
    if keep {
        world.keep();
    }
    let exe = crate::paths::paths().exe.to_string_lossy().into_owned();
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.clone(),
            headless,
            args: [
                "--columns",
                &COLUMNS.to_string(),
                "--lines",
                &LINES.to_string(),
                "-e",
                &exe,
                "presence-standin",
            ]
            .map(str::to_string)
            .to_vec(),
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    inst.client = client.clone();
    println!(
        "== {} client {} ; {} instance pid={} sock={}",
        aterm.display(),
        client.display(),
        if headless { "headless" } else { "windowed" },
        inst.pid,
        inst.sock.display()
    );
    let mut p = Probe {
        inst,
        last: 0,
        base_flaps: String::new(),
        base_risks: String::new(),
        oks: 0,
        fails: 0,
    };
    let finish = |p: &Probe| -> Res {
        println!("\n{} ok, {} FAIL", p.oks, p.fails);
        Ok(if p.fails > 0 { 1 } else { 0 })
    };
    if let Err(f) = p.inst.await_socket(instance::BOOT) {
        let why = f.msg.unwrap_or_default();
        p.row(false, "boot: the instance answers its socket", &why);
        return finish(&p);
    }
    if p.ctl(&["await", "match", "bypass.permissions", "timeout=60000"])
        .code
        != 0
    {
        p.row(
            false,
            "boot: the stand-in paints its frame",
            "no footer on screen in 60 s",
        );
        return finish(&p);
    }
    sleep(1.5)?;
    let pid = p.inst.pid;
    p.row(
        true,
        "boot: the instance is up, the stand-in painted",
        &format!("pid={pid}"),
    );
    let base = p.status();
    let (head, why_not) = match p.read_resizes(false) {
        Ok((head, _)) => (head, "the header names no flaps= or risks=".to_string()),
        Err(why) => (String::new(), why),
    };
    p.base_flaps = field(&head, "flaps").unwrap_or_default();
    p.base_risks = field(&head, "risks").unwrap_or_default();
    println!(
        "      baseline: render={} resizes={} flaps={} risks={} (boot sizing)",
        field(&base, "render").unwrap_or_default(),
        field(&base, "resizes").unwrap_or_default(),
        p.base_flaps,
        p.base_risks
    );
    if p.base_flaps.is_empty() || p.base_risks.is_empty() {
        p.row(false, "baseline: the resizes ledger answers", &why_not);
        p.base_flaps = "0".into();
        p.base_risks = "0".into();
    }
    for phase in &phases {
        match phase {
            'A' => p.phase_a()?,
            'H' => p.phase_h()?,
            'B' => p.phase_b()?,
            'D' => p.phase_d()?,
            _ => p.phase_t(trials)?,
        }
    }
    p.end(LINES)?;
    finish(&p)
}

/// Claude Code's shape on the alternate screen.
#[must_use]
pub fn frame(rows: u16, cols: u16, composer: &str) -> String {
    let at = rows.saturating_sub(5);
    let rule = "─".repeat(usize::from(cols));
    let mut lines: Vec<String> = (0..at.saturating_sub(1))
        .map(|r| format!("  ⏺ transcript line {r:02}"))
        .collect();
    lines.extend([
        rule.clone(),
        format!("> {composer}"),
        rule,
        "  ⏵⏵ bypass permissions on (shift+tab to cycle)".into(),
        "  1 shell · esc to interrupt".into(),
        "  ◯ context 12% · 1.5m tokens".into(),
    ]);
    let mut s = String::from("\x1b[?2026h\x1b[2J");
    for (r, text) in lines.iter().enumerate() {
        s.push_str(&format!("\x1b[{};1H{text}", r + 1));
    }
    s.push_str(&format!(
        "\x1b[{};{}H\x1b[?2026l",
        at + 1,
        3 + composer.chars().count()
    ));
    s
}

struct Standin {
    /// The size the program believes: what it last read.
    seen: (u16, u16),
    composer: String,
}

fn size() -> (u16, u16) {
    crate::sys::winsize(1).map_or((24, 80), |w| (w.ws_row, w.ws_col))
}

fn put(s: &str) {
    let _ = crate::sys::write_all(1, s.as_bytes());
}

impl Standin {
    fn diff(&self) {
        let at = self.seen.0.saturating_sub(5);
        put(&format!(
            "\x1b[?2026h\x1b[{};3H{}\x1b[K\x1b[?2026l",
            at + 1,
            self.composer
        ));
    }

    /// Node's SIGWINCH rule: a full repaint only on a CHANGED size.
    fn read_size(&mut self) {
        let now = size();
        if now == self.seen {
            self.diff();
        } else {
            self.seen = now;
            put(&frame(now.0, now.1, &self.composer));
        }
    }
}

/// The stand-in's `main` (`aterm-lab presence-standin`, the tab's program).
#[must_use]
pub fn standin_main(_args: &[String]) -> i32 {
    let Ok(winch) = crate::sys::SignalPipe::install(&[libc::SIGWINCH]) else {
        return 1;
    };
    let _raw = crate::sys::RawMode::enter(0);
    let seen = size();
    let mut s = Standin {
        seen,
        composer: String::new(),
    };
    put(&format!("\x1b[?1049h{}", frame(seen.0, seen.1, "")));
    loop {
        let mut fds = [crate::sys::pollfd(0), crate::sys::pollfd(winch.read_fd)];
        let _ = crate::sys::poll(&mut fds, -1);
        if fds[1].revents != 0 && !winch.drain().is_empty() {
            s.read_size();
        }
        if fds[0].revents == 0 {
            continue;
        }
        let mut b = [0u8; 1];
        let n = crate::sys::read(0, &mut b).unwrap_or(0);
        match (n, b[0]) {
            (0, _) | (_, b'q') => std::process::exit(0),
            (_, b'\r' | b'\n') => {
                s.composer.clear();
                s.diff();
            }
            (_, c @ 0x20..=0x7e) => {
                s.composer.push(char::from(c));
                s.diff();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
