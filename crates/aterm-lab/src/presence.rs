// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `presence-flap --window`.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::instance::{self, CONFIG_OFF, Instance, Launch, Reply, World};
use crate::screen::field;
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

/// `aterm_messages::chrome::CHROME_REGROW_SETTLE`.
pub const FLOOR_MS: u64 = 400;

/// The steps, in the order they run.
pub const PHASES: [char; 6] = ['A', 'H', 'B', 'D', 'T', 'X'];

/// The cross-source schedules of phase X.
pub const SCHEDULES: [&str; 2] = ["strip-rise-then-bandshrink", "band-rise-then-stripfall"];

/// How long before the band's quiet shrink is due phase X asks the strip's rise
/// (`strip-rise-then-bandshrink`): instants swept, not a budget.
const LEADS_MS: [u64; 3] = [350, 100, 20];

/// How long a rise nothing has taken back is waited for before phase X calls its
/// fall lost: a hang detector, never a budget (the floor is a least gap).
const HANG: Duration = Duration::from_mins(1);

fn sleep(secs: f64) -> Result<(), Stopped> {
    stop::sleep(Duration::from_secs_f64(secs))
}

/// One resize row of the ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resize {
    pub id: u64,
    pub t: u64,
    pub from_rows: u32,
    pub to_rows: u32,
}

/// A `resize <id> t=<ms> … from=<CxR> to=<CxR> …` row.
#[must_use]
pub fn parse_resize(line: &str) -> Option<Resize> {
    let rest = line.strip_prefix("resize ")?;
    let id = rest.split_whitespace().next()?.parse().ok()?;
    let rows = |key: &str| -> Option<u32> { field(line, key)?.split_once('x')?.1.parse().ok() };
    Some(Resize {
        id,
        t: field(line, "t")?.parse().ok()?,
        from_rows: rows("from")?,
        to_rows: rows("to")?,
    })
}

/// Each shrink paired with the grow that takes it back (the newest open shrink of the opposite sizes), as the gap between them in ms.
#[must_use]
pub fn pair_gaps(rows: &[Resize]) -> Vec<u64> {
    let mut open: Vec<Resize> = Vec::new();
    let mut gaps = Vec::new();
    for r in rows {
        if r.to_rows < r.from_rows {
            open.push(*r);
        } else if r.to_rows > r.from_rows
            && let Some(at) = open
                .iter()
                .rposition(|s| s.from_rows == r.to_rows && s.to_rows == r.from_rows)
        {
            let s = open.remove(at);
            gaps.push(r.t.saturating_sub(s.t));
        }
    }
    gaps
}

/// What one phase X schedule's resize rows (those after its base) show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Measured {
    /// Each shrink and the grow that took it back, ms apart.
    Pairs(Vec<u64>),
    /// No pair formed because the schedule missed its instant.
    Missed(String),
    /// A rise re-gridded the window and nothing took it back.
    Stuck(String),
}

/// Reads one schedule's resize rows in the ledger's own order, so the verdict never rests on how fast the host ran the schedule.
#[must_use]
pub fn measure(rows: &[Resize]) -> Measured {
    let gaps = pair_gaps(rows);
    if !gaps.is_empty() {
        return Measured::Pairs(gaps);
    }
    let shrink = rows.iter().find(|r| r.to_rows < r.from_rows);
    let grow = rows.iter().find(|r| r.to_rows > r.from_rows);
    match (shrink, grow) {
        (Some(s), Some(g)) if g.id < s.id => Measured::Missed(format!(
            "the fall landed first (resize {} t={}), the rise {} ms after it (resize {} t={})",
            g.id,
            g.t,
            s.t.saturating_sub(g.t),
            s.id,
            s.t
        )),
        (Some(s), _) => Measured::Stuck(format!(
            "resize {} t={} rows {} -> {}: no grow took it back",
            s.id, s.t, s.from_rows, s.to_rows
        )),
        (None, _) => Measured::Missed(format!(
            "no rise re-gridded the window ({} resize rows)",
            rows.len()
        )),
    }
}

/// A phase X sweep's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every pair landed the floor apart.
    Held(u64),
    /// The closest pair, in ms, is under the floor.
    Under(u64),
    /// A rise nothing took back.
    Stuck(String),
    /// No run formed a pair.
    NotMeasured(String),
}

/// Judges a sweep (one or more runs of a schedule) whole.
#[must_use]
pub fn judge_sweep(runs: &[Measured]) -> Verdict {
    let mut gaps = Vec::new();
    let mut missed = Vec::new();
    for m in runs {
        match m {
            Measured::Pairs(g) => gaps.extend_from_slice(g),
            Measured::Missed(why) => missed.push(why.as_str()),
            Measured::Stuck(why) => return Verdict::Stuck(why.clone()),
        }
    }
    match gaps.iter().min().copied() {
        Some(min) if min + 1 >= FLOOR_MS => Verdict::Held(min),
        Some(min) => Verdict::Under(min),
        None => Verdict::NotMeasured(format!("no pair formed: {}", missed.join("; "))),
    }
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
            return Err(format!("unknown phase: {c} (A, H, B, D, T, X)"));
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

/// A `resizes` reply's rows.
///
/// # Errors
/// The reply failed or carried no `OK` header: its words ([`failed`]), never an
/// empty ledger.
fn ledger_rows(asked: &str, r: &Reply) -> Result<Vec<Resize>, String> {
    if r.code != 0 || r.header().is_empty() {
        return Err(failed(asked, r));
    }
    Ok(r.stdout.lines().filter_map(parse_resize).collect())
}

/// One held control connection: a schedule's requests go out milliseconds
/// apart, or pipelined in one write.
struct Held {
    w: aterm_uds::CtlStream,
    r: std::io::BufReader<aterm_uds::CtlStream>,
}

impl Held {
    fn open(inst: &Instance) -> Result<Held, String> {
        let client = inst.connect()?;
        let w = client
            .transport()
            .try_clone()
            .map_err(|e| format!("cannot share the connection: {e}"))?;
        let r = w
            .try_clone()
            .map_err(|e| format!("cannot share the connection: {e}"))?;
        let _ = r.set_read_timeout(Some(Duration::from_secs(60)));
        Ok(Held {
            w,
            r: std::io::BufReader::new(r),
        })
    }

    fn send(&mut self, lines: &[&str]) -> Result<(), String> {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        self.w
            .write_all(text.as_bytes())
            .map_err(|e| format!("the control connection: {e}"))
    }

    fn line(&mut self) -> Result<String, String> {
        let mut l = String::new();
        match self.r.read_line(&mut l) {
            Ok(0) => Err("the control socket closed".into()),
            Ok(_) => Ok(l.trim_end_matches(['\r', '\n']).to_string()),
            Err(e) => Err(format!("the control connection: {e}")),
        }
    }

    fn req(&mut self, line: &str) -> Result<String, String> {
        self.send(&[line])?;
        self.line()
    }
}

/// Busy-wait to `t`: a schedule's instants are milliseconds wide.
fn until(t: Instant) -> Result<(), Stopped> {
    while Instant::now() < t {
        stop::check()?;
        std::thread::sleep(Duration::from_micros(500));
    }
    Ok(())
}

fn message_id(reply: &str) -> Option<String> {
    field(reply, "message").filter(|m| m.chars().all(|c| c.is_ascii_digit()))
}

/// The band row phase X posts.
const BAND: &str = "notice post probe sev=warn Probe band row";

struct Probe {
    inst: Instance,
    /// The newest resize row id seen.
    last: u64,
    base_flaps: String,
    base_risks: String,
    oks: u32,
    fails: u32,
    unmeasured: u32,
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

    fn not_measured(&mut self, what: &str, why: &str) {
        self.unmeasured += 1;
        println!("{:<4}  {what:<60}  NOT MEASURED: {why}", "--");
    }

    fn status(&self) -> String {
        self.ctl(&["status"]).first()
    }

    /// `resizes since=<since>` (every retained row at 0): the reply and its
    /// resize rows.
    ///
    /// # Errors
    /// The reply gave no reading ([`ledger_rows`]).
    fn resizes(&self, since: u64) -> Result<(Reply, Vec<Resize>), String> {
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
        if let Some(m) = rows.iter().map(|x| x.id).max() {
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

    /// The newest resize row's id now (0: the ledger holds none).
    ///
    /// # Errors
    /// The reply gave no reading ([`ledger_rows`]): never base 0, which would
    /// measure the whole ledger.
    fn newest_resize(&self) -> Result<u64, String> {
        let (_, rows) = self.resizes(0)?;
        Ok(rows.iter().map(|r| r.id).max().unwrap_or(0))
    }

    /// What the resize rows after `base` show, each row and pair printed. A rise
    /// nothing has taken back yet is waited for, up to [`HANG`]. A read that
    /// gives no reading is passed over: the last reading stands and the ledger
    /// is polled again in the same wait. The inner `Err`: every read in it failed.
    fn measured_since(&self, base: u64) -> Result<Result<Measured, String>, Stopped> {
        let settled = |read: &Result<Vec<Resize>, String>| matches!(read, Ok(rows) if !matches!(measure(rows), Measured::Stuck(_)));
        let deadline = Instant::now() + HANG;
        let mut read = self.resizes(base).map(|(_, rows)| rows);
        while !settled(&read) && Instant::now() < deadline {
            sleep(0.25)?;
            let next = self.resizes(base).map(|(_, rows)| rows);
            if next.is_ok() || read.is_err() {
                read = next;
            }
        }
        let rows = match read {
            Ok(rows) => rows,
            Err(why) => {
                return Ok(Err(format!(
                    "every read in {} s failed, the last: {why}",
                    HANG.as_secs()
                )));
            }
        };
        for r in &rows {
            println!(
                "      resize {} t={} rows {} -> {}",
                r.id, r.t, r.from_rows, r.to_rows
            );
        }
        let m = measure(&rows);
        match &m {
            Measured::Pairs(gaps) => {
                for g in gaps {
                    println!("      pair: shrink and its grow back {g} ms apart");
                }
            }
            Measured::Missed(why) => println!("      no pair: {why}"),
            Measured::Stuck(why) => println!("      no pair: {why} in {} s", HANG.as_secs()),
        }
        Ok(Ok(m))
    }

    /// The tab strip rises `lead` before the band's quiet shrink falls due (its
    /// `SHRINK_QUIET`, 1.5 s past the dismiss): the band's fall must wait the
    /// floor past the strip's rise.
    fn strip_rise_then_bandshrink(
        &mut self,
        lead: Duration,
    ) -> Result<Result<Measured, String>, Stopped> {
        let mut c = match Held::open(&self.inst) {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        let posted = match c.req(BAND) {
            Ok(p) => p,
            Err(e) => return Ok(Err(e)),
        };
        println!("      notice post: {posted}");
        let Some(mid) = message_id(&posted) else {
            return Ok(Err(format!("no message id in {posted:?}")));
        };
        sleep(0.8)?;
        let base = match self.newest_resize() {
            Ok(base) => base,
            Err(why) => {
                let r = c
                    .req(&format!("notice dismiss {mid}"))
                    .unwrap_or_else(|e| e);
                println!("      cleanup: dismiss {mid}: {r}");
                sleep(2.5)?;
                return Ok(Err(format!("the base read: {why}")));
            }
        };
        let t0 = Instant::now();
        let r = c
            .req(&format!("notice dismiss {mid}"))
            .unwrap_or_else(|e| e);
        println!("      dismiss: {r}");
        until(t0 + Duration::from_millis(1500) - lead)?;
        let at = t0.elapsed();
        let r = c.req("settings set tab_strip_rows 1").unwrap_or_else(|e| e);
        println!(
            "      strip rise asked at dismiss+{:.1} ms, answered at +{:.1} ms: {r}",
            at.as_secs_f64() * 1000.0,
            t0.elapsed().as_secs_f64() * 1000.0
        );
        sleep(2.5)?;
        let m = self.measured_since(base)?;
        let r = c.req("settings unset tab_strip_rows").unwrap_or_else(|e| e);
        println!("      cleanup: strip down: {r}");
        sleep(2.5)?;
        Ok(m)
    }

    /// A band row rises and the tab strip falls on the next request (one write):
    /// the strip's fall must wait the floor past the band's rise.
    fn band_rise_then_stripfall(&mut self) -> Result<Result<Measured, String>, Stopped> {
        let mut c = match Held::open(&self.inst) {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        let r = c.req("settings set tab_strip_rows 1").unwrap_or_else(|e| e);
        println!("      strip up: {r}");
        sleep(2.5)?;
        let base = match self.newest_resize() {
            Ok(base) => base,
            Err(why) => {
                let r = c.req("settings unset tab_strip_rows").unwrap_or_else(|e| e);
                println!("      cleanup: strip down: {r}");
                sleep(2.5)?;
                return Ok(Err(format!("the base read: {why}")));
            }
        };
        let t0 = Instant::now();
        if let Err(e) = c.send(&[BAND, "settings unset tab_strip_rows"]) {
            return Ok(Err(e));
        }
        let (posted, unset) = match (c.line(), c.line()) {
            (Ok(p), Ok(u)) => (p, u),
            (Err(e), _) | (_, Err(e)) => return Ok(Err(e)),
        };
        println!(
            "      notice post: {posted} ; strip down: {unset} (one write, both answered at +{:.1} ms)",
            t0.elapsed().as_secs_f64() * 1000.0
        );
        sleep(2.5)?;
        let m = self.measured_since(base)?;
        if let Some(mid) = message_id(&posted) {
            let r = c
                .req(&format!("notice dismiss {mid}"))
                .unwrap_or_else(|e| e);
            println!("      cleanup: dismiss {mid}: {r}");
        }
        sleep(2.5)?;
        Ok(m)
    }

    fn judge(&mut self, case: &str, ran: Result<Vec<Measured>, String>) {
        let apart = format!("X {case}: the pair lands the floor apart");
        match ran.map(|runs| judge_sweep(&runs)) {
            Err(why) => self.row(false, &format!("X {case}: the schedule ran"), &why),
            Ok(Verdict::Held(min)) => {
                self.row(
                    true,
                    &apart,
                    &format!("min gap {min} ms (floor {FLOOR_MS} ms)"),
                );
            }
            Ok(Verdict::Under(min)) => {
                self.row(
                    false,
                    &apart,
                    &format!("min gap {min} ms (floor {FLOOR_MS} ms)"),
                );
            }
            Ok(Verdict::Stuck(why)) => {
                self.row(false, &format!("X {case}: every rise is taken back"), &why);
            }
            Ok(Verdict::NotMeasured(why)) => self.not_measured(&apart, &why),
        }
    }

    fn phase_x(&mut self) -> Result<(), Stopped> {
        println!(
            "---- X: cross-source chrome pairs, the stand-in busy (size read 20 ms late, a glyph every 5 ms)"
        );
        self.quiet()?;
        let _ = self.read_resizes(false);
        println!("      busy: {}", verdict(&self.ctl(&["ctrl", "n"])));
        sleep(0.5)?;
        let mut swept = Ok(Vec::new());
        for lead_ms in LEADS_MS {
            println!(
                "    -- {} (rise asked {lead_ms} ms before the shrink)",
                SCHEDULES[0]
            );
            let ran = self.strip_rise_then_bandshrink(Duration::from_millis(lead_ms))?;
            match (&mut swept, ran) {
                (Ok(all), Ok(m)) => all.push(m),
                (Ok(_), Err(why)) => swept = Err(why),
                (Err(_), _) => {}
            }
        }
        let leads = LEADS_MS.map(|ms| ms.to_string()).join(", ");
        self.judge(&format!("{} (leads {leads} ms)", SCHEDULES[0]), swept);
        println!("    -- {}", SCHEDULES[1]);
        let ran = self.band_rise_then_stripfall()?;
        self.judge(SCHEDULES[1], ran.map(|m| vec![m]));
        println!("      quiet: {}", verdict(&self.ctl(&["ctrl", "o"])));
        sleep(0.5)?;
        let _ = self.read_resizes(false);
        self.no_new_flap("X");
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

/// `presence-flap --window|--headless [--aterm PATH] [--client PATH] [--phases "A H B D T X"] [--trials N] [--keep]`.
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
        None => PHASES.into_iter().filter(|&p| window || p != 'X').collect(),
    };
    if headless && phases.contains(&'X') {
        return Err(Fail::usage(
            "phase X judges the window's chrome re-grids: it needs --window".to_string(),
        ));
    }
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
        unmeasured: 0,
    };
    let finish = |p: &Probe| -> Res {
        println!(
            "\n{} ok, {} FAIL, {} not measured",
            p.oks, p.fails, p.unmeasured
        );
        Ok(if p.fails > 0 {
            1
        } else if p.unmeasured > 0 {
            3
        } else {
            0
        })
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
            'T' => p.phase_t(trials)?,
            _ => p.phase_x()?,
        }
    }
    p.end(LINES)?;
    finish(&p)
}

/// How late a BUSY stand-in reads its size after a SIGWINCH.
const LAG: Duration = Duration::from_millis(20);
/// How often a BUSY stand-in writes its spinner glyph.
const TICK: Duration = Duration::from_millis(5);
const GLYPHS: [char; 4] = ['|', '/', '-', '\\'];

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
    busy: bool,
    read_at: Option<Instant>,
    spin_at: Instant,
    spin: usize,
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

    fn on_winch(&mut self) {
        if self.busy {
            self.read_at.get_or_insert_with(|| Instant::now() + LAG);
        } else {
            self.read_size();
        }
    }

    /// The status cell against the size the program BELIEVES, then the cursor
    /// back to the composer: one write, no erase.
    fn spin(&mut self) {
        self.spin += 1;
        put(&format!(
            "\x1b[{};3H{}\x1b[{};{}H",
            self.seen.0.saturating_sub(1),
            GLYPHS[self.spin % 4],
            self.seen.0.saturating_sub(4),
            3 + self.composer.chars().count()
        ));
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
        busy: false,
        read_at: None,
        spin_at: Instant::now(),
        spin: 0,
    };
    put(&format!("\x1b[?1049h{}", frame(seen.0, seen.1, "")));
    loop {
        let timeout = if s.busy {
            let due = s.read_at.map_or(s.spin_at, |r| r.min(s.spin_at));
            let ms = due
                .saturating_duration_since(Instant::now())
                .as_micros()
                .div_ceil(1000);
            i32::try_from(ms).unwrap_or(i32::MAX)
        } else {
            -1
        };
        let mut fds = [crate::sys::pollfd(0), crate::sys::pollfd(winch.read_fd)];
        let _ = crate::sys::poll(&mut fds, timeout);
        if fds[1].revents != 0 && !winch.drain().is_empty() {
            s.on_winch();
        }
        let now = Instant::now();
        if s.read_at.is_some_and(|t| now >= t) {
            s.read_at = None;
            s.read_size();
        }
        if s.busy && now >= s.spin_at {
            s.spin();
            s.spin_at = now + TICK;
        }
        if fds[0].revents == 0 {
            continue;
        }
        let mut b = [0u8; 1];
        let n = crate::sys::read(0, &mut b).unwrap_or(0);
        match (n, b[0]) {
            (0, _) | (_, b'q') => std::process::exit(0),
            (_, 0x0e) => {
                s.busy = true;
                s.spin_at = now;
            }
            (_, 0x0f) => {
                s.busy = false;
                if s.read_at.take().is_some() {
                    s.read_size();
                }
            }
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
mod tests {
    use super::*;

    fn rz(id: u64, t: u64, from_rows: u32, to_rows: u32) -> Resize {
        Resize {
            id,
            t,
            from_rows,
            to_rows,
        }
    }

    #[test]
    fn a_resize_row_reads_its_id_time_and_rows() {
        let line = "resize 7 t=1234 run=2 ord=3 from=80x24 to=80x23 alt=1 sync=0 site=chrome at=- win=80x24 chrome=1";
        assert_eq!(parse_resize(line), Some(rz(7, 1234, 24, 23)));
        assert_eq!(parse_resize("run 2 t=1 n=2"), None);
    }

    #[test]
    fn phases_parse_in_any_spelling_and_refuse_strangers() {
        assert_eq!(parse_phases("A H").unwrap(), vec!['A', 'H']);
        assert_eq!(parse_phases("b,t").unwrap(), vec!['B', 'T']);
        assert_eq!(parse_phases("x").unwrap(), vec!['X']);
        assert!(parse_phases("Q").is_err());
    }

    #[test]
    fn a_shrink_pairs_with_the_grow_that_takes_it_back() {
        let rows = [rz(1, 100, 24, 23), rz(2, 500, 23, 24)];
        assert_eq!(pair_gaps(&rows), vec![400]);
        assert_eq!(measure(&rows), Measured::Pairs(vec![400]));
        let rows = [rz(3, 1000, 23, 22), rz(4, 1006, 22, 23)];
        assert_eq!(pair_gaps(&rows), vec![6]);
        assert_eq!(
            pair_gaps(&[rz(5, 0, 23, 24), rz(6, 9, 24, 23)]),
            Vec::<u64>::new()
        );
    }

    #[test]
    fn a_sweep_fails_only_on_what_the_ledger_shows_wrong() {
        let held = Measured::Pairs(vec![401, 400]);
        let missed = measure(&[rz(10, 118_022, 23, 24), rz(11, 118_047, 24, 23)]);
        assert!(
            matches!(&missed, Measured::Missed(why) if why.contains("25 ms after")),
            "{missed:?}"
        );
        let under = measure(&[rz(14, 27_325, 23, 22), rz(15, 27_363, 22, 23)]);
        assert_eq!(under, Measured::Pairs(vec![38]));
        let stuck = measure(&[rz(2, 103_312, 23, 22)]);
        assert!(matches!(stuck, Measured::Stuck(_)), "{stuck:?}");

        assert_eq!(
            judge_sweep(&[held.clone(), missed.clone()]),
            Verdict::Held(400)
        );
        assert_eq!(
            judge_sweep(&[Measured::Pairs(vec![399])]),
            Verdict::Held(399),
            "1 ms is the ledger's rounding"
        );
        assert_eq!(judge_sweep(&[held.clone(), under]), Verdict::Under(38));
        assert!(matches!(judge_sweep(&[held, stuck]), Verdict::Stuck(_)));
        let none = measure(&[]);
        assert!(matches!(none, Measured::Missed(_)), "{none:?}");
        assert!(matches!(
            judge_sweep(&[missed.clone(), missed, none]),
            Verdict::NotMeasured(why) if why.starts_with("no pair formed: the fall landed first")
        ));
    }

    #[test]
    fn the_stand_in_frame_fills_the_screen_and_parks_on_the_composer() {
        let f = frame(24, 80, "ab");
        assert_eq!(f.matches(";1H").count(), 24, "one cursor move per row");
        assert!(f.contains("\x1b[1;1H  ⏺ transcript line 00"));
        assert!(f.contains("\x1b[24;1H  ◯ context 12%"));
        assert!(f.ends_with("\x1b[20;5H\x1b[?2026l"));
    }

    #[test]
    fn a_verdict_is_the_first_stderr_line_without_the_client_prefix() {
        let r = Reply {
            code: 0,
            stdout: "$ hi\n".into(),
            stderr: "aterm-ctl: turn submitted=1 status=settled\n$ hi\n".into(),
        };
        assert_eq!(verdict(&r), "turn submitted=1 status=settled");
        let r = Reply {
            code: 0,
            stdout: "OK lease released\n".into(),
            stderr: String::new(),
        };
        assert_eq!(verdict(&r), "OK lease released");
    }

    #[test]
    fn a_resizes_reply_that_failed_is_no_reading_never_an_empty_ledger() {
        let ok = Reply {
            code: 0,
            stdout: "resize 7 t=1234 run=2 ord=3 from=80x24 to=80x23 alt=1\nrun 2 t=1 n=1\n".into(),
            stderr: "aterm-ctl: OK 2 total=7 runs=2 flaps=0 risks=0 render=ok\n".into(),
        };
        assert_eq!(
            ledger_rows("resizes since=6", &ok),
            Ok(vec![rz(7, 1234, 24, 23)])
        );
        let none = Reply {
            stderr: "OK 0 total=7 runs=2 flaps=0 risks=0 render=ok\n".into(),
            ..Reply::default()
        };
        assert_eq!(ledger_rows("resizes since=7", &none), Ok(Vec::new()));

        let refused = Reply {
            code: 1,
            stdout: String::new(),
            stderr: "aterm-ctl: ERR usage: resizes [<n>] [since=<id>]\n".into(),
        };
        let why = ledger_rows("resizes since=x", &refused).unwrap_err();
        assert!(why.contains("ERR usage"), "{why}");
        let timed_out = Reply { code: 124, ..ok };
        assert!(ledger_rows("resizes since=6", &timed_out).is_err());
        assert!(ledger_rows("resizes since=6", &Reply::default()).is_err());
    }
}
