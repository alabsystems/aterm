// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-alab-proof --window-only`: codex-alab's window (design §14.2's 2d, landing with 8b).
//! Settings ▸ Harness in a window of its own and four codex-alab tabs in the window in front, on
//! the proof's rig ([`surfaces::lay_windows_by`], as the forever lab lays its desk), read through
//! the forever lab's own readers ([`crate::forever::surfaces`]) as each reaches its scene: `land`
//! under Land through its nudges to its landing, `reland` to its landing, the `off` leg once
//! codex-alab gave its claim back for Autopilot Off, and a `worker` its manager's tab spawned under
//! Land. At each read, the tab's `chrome` `autopilot` line, its pad (the row over the composer's
//! prompt, as the program drew it), its state line on the page, its hover in the strip, This Mac's
//! row, its menu bar rows and its `status`; then, all four at rest, END's idle window twice, the
//! page open and then shut ([`crate::forever::surfaces::idle_counts`]).
//!
//! What codex-alab tells is judged by its build ([`dev_build`]): from [`TELLS_LAND_HOLD`] on, its
//! Land hold must reach the status, the state line, the hover and This Mac's `landing` count, and
//! leave them once landed, and the worker's row must read `landing` before `landed`; from
//! [`SAYS_ITS_MODE`] on, its own line must name the face. An older build's missing half is a
//! named skip that names the build, never a pass on a read that missed it.
//!
//! Land and reland each run a slow turn ([`model::SLOW_TURN`]), past the work floor (§11.2.3), so
//! each landing is an end (`status end=`) with its Done for you row; the worker's few seconds are
//! under it, so its row under its manager is what it shows. The worker's land act is answered
//! only once the pass read that row say `landing` ([`model::hold_worker_land`]), so its hold
//! stands until it is seen, not for the second or two a 2 s read catches by luck.
//!
//! Codex owns the prompt area. The host must report `why=no-footer` even when the
//! native program leaves a blank pad. Builds with native mode text must show their
//! own expected face there; any host-drawn Codex chip fails this check.

use super::*;
use crate::forever::surfaces::{self, Desk, Glance, StripTab};

/// The pass's hang detector: the land leg's four nudges and its landing, each a slow turn and a
/// back-off, with room. A wait for what must happen, never a budget.
const HANG: Duration = Duration::from_secs(30 * 60);

/// The hang detector of a landing's Done for you row once `status end=` told the end (or, on an
/// aterm whose `status` prints no `end=`, once the landing showed).
const ROW_HANG: Duration = Duration::from_secs(60);

/// How long every leg's journal must stand still before an idle window is taken.
const QUIET: Duration = Duration::from_secs(20);

/// How often the pass reads the window.
const POLL: Duration = Duration::from_secs(2);

/// The deadline owner of Settings ▸ Harness's next turn of its words while the page is open
/// (`App::note_land_at_offer`: its day words at midnight, or a timed Land's offer at its minute),
/// hours past END's idle window. With the page open and every leg at rest it is the loop's only
/// deadline, so it arms at each wake; it never fires in the window.
const PAGE_TURN: &str = "autopilot_timer";

/// What one tab must show, and how its window is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scene {
    /// Under Land, before its landing: the Land face, its hold told, then landed.
    Land,
    /// Landed.
    Reland,
    /// Autopilot Off, codex-alab's claim given back: no chip.
    Off,
    /// A worker its manager spawned under Land: its row under the manager goes from landing to
    /// landed.
    Worker,
}

/// What the package tells, by its build.
struct Build {
    /// `alab-build.json`'s version.
    version: String,
    /// Whether it tells its Land hold ([`TELLS_LAND_HOLD`]).
    tells_hold: bool,
    /// Whether its own line names its mode ([`SAYS_ITS_MODE`]).
    says_mode: bool,
}

impl Build {
    fn of(version: String) -> Self {
        let build = dev_build(&version);
        Build {
            tells_hold: build.is_some_and(|b| b >= TELLS_LAND_HOLD),
            says_mode: build.is_some_and(|b| b >= SAYS_ITS_MODE),
            version,
        }
    }
}

/// What a leg's window showed, each distinct reading once, in order, with its time from the
/// pass's start (ms).
#[derive(Default)]
struct Seen {
    /// Every read's surfaces, each distinct one.
    reads: Vec<(i64, Read)>,
    /// The latest read's time, whether or not it showed anything new.
    last_t: i64,
    /// Its menu bar rows (`menu_of`), each distinct one.
    rows: Vec<(i64, String)>,
    /// Its row under its manager (`worker_of`), each distinct one.
    under: Vec<(i64, String)>,
}

/// One read of a leg's window.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Read {
    /// `chrome`'s `autopilot` line for the tab, after `sid=`; empty where none.
    line: String,
    /// `status`'s `autopilot`, `task`, `land_held`, `land_since`, `supervisor_proven` and `end`
    /// words, `?` where it printed none.
    words: String,
    /// The pad: the row over the composer's prompt as the program drew it, trimmed; `None` where
    /// the screen showed no prompt.
    pad: Option<String>,
    /// Its state line on Settings ▸ Harness ([`state_of`]): `-` where the page shows no row for
    /// it, `?` where the page was not read.
    state: String,
    /// Its hover in the strip (`driven by aterm (…)`): `-` where it says none, `?` where the
    /// strip's tips were not read whole.
    hover: String,
    /// This Mac's row in the menu bar: `-` where none.
    here: String,
}

impl Read {
    fn word(&self, key: &str) -> Option<String> {
        field(&self.words, key)
    }

    /// Whether its status shows codex-alab's Land hold told (`land_since=`).
    fn held(&self) -> bool {
        self.word("land_since")
            .is_some_and(|v| v != "-" && v != "?")
    }

    /// Whether that hold has a yield judged (`land_held=`, codex-alab's `read=1`).
    fn judged(&self) -> bool {
        self.held() && self.word("land_held").is_some_and(|v| v != "-" && v != "?")
    }

    fn landed(&self) -> bool {
        self.word("task").as_deref() == Some("landed")
    }
}

/// The `state=` line [`surfaces::page_of`] gives tab `tab` on Settings ▸ Harness's `rows`; `-`
/// where none.
fn state_of(rows: &[(String, String)], tab: &str) -> String {
    surfaces::page_of(rows, tab)
        .and_then(|p| {
            p.lines()
                .find_map(|l| l.strip_prefix("state="))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "-".into())
}

/// Codex owns the prompt area. The host draws no chip or switch;
/// builds that show a native mode must show the expected face in their own line.
fn judge_line(
    line: &str,
    pad: Option<&str>,
    face: &str,
    says_mode: bool,
) -> Result<String, String> {
    if line.is_empty() {
        return Err("chrome printed no autopilot line for the tab".into());
    }
    let c = surfaces::chip(line);
    if c.drawn.is_some() || c.why.as_deref() != Some("no-footer") {
        return Err(format!(
            "aterm drew Codex controls instead of leaving the native UI alone: {line}"
        ));
    }
    if says_mode {
        let own = pad
            .filter(|p| !p.is_empty())
            .ok_or("codex-alab drew no native controls")?;
        if let Some(cells) = surfaces::face_words(face)
            && !own.contains(cells)
        {
            return Err(format!("codex-alab's own line names no {face} ({own:?})"));
        }
    }
    Ok(format!(
        "codex-alab owns its controls; face={face}, no host footer"
    ))
}

/// Whether `entry` ([`surfaces::worker_of`]'s) says the worker is landing, or landed.
fn under_says(entry: &str, word: &str) -> bool {
    entry
        .lines()
        .find_map(|l| l.strip_prefix("row|"))
        .is_some_and(|row| row.contains(&format!("\u{2014} {word}")))
}

fn push_new<T: PartialEq>(list: &mut Vec<(i64, T)>, t: i64, v: T) {
    if list.last().is_none_or(|(_, last)| *last != v) {
        list.push((t, v));
    }
}

/// `ctl` on the rig's instance as [`surfaces::lay_windows_by`] takes it.
fn rig_ctl(proof: &Proof, args: &[&str]) -> surfaces::CtlReply {
    let r = proof.inst.ctl(args);
    Ok((r.code, r.stdout, r.stderr))
}

/// The window pass: Settings ▸ Harness and the legs' window laid, its four legs opened, read
/// until each reached its scene or [`HANG`] ran out, judged, then the idle windows.
pub(super) fn legs(proof: &mut Proof, pkg: &Path) -> Result<(), Fail> {
    let bus = match proof.ready_rig(pkg) {
        Ok((bus, _)) => bus,
        Err(Unready::Unlinked(why)) => {
            proof.check(&format!("the package is linked ({why})"), false);
            return Ok(());
        }
        Err(Unready::NoSocket(f)) => {
            proof.check("the headless aterm answers its socket", false);
            return Err(f);
        }
        Err(Unready::Stopped(stopped)) => return Err(stopped.into()),
    };
    proof.check("window: the lab's bus is up for the window pass", bus);
    let b = Build::of(proof.package_version());
    let yes = |v: bool| if v { "yes" } else { "no" };
    println!(
        "window pass on codex-alab {}: tells its Land hold {} (from build {TELLS_LAND_HOLD}), \
         names its mode on its line {} (from build {SAYS_ITS_MODE})",
        b.version,
        yes(b.tells_hold),
        yes(b.says_mode)
    );
    let desk = match surfaces::lay_windows_by(&|args| rig_ctl(proof, args)) {
        Ok(desk) => desk,
        Err(why) => {
            proof.check(
                &format!("window: Settings ▸ Harness and the legs' window laid ({why})"),
                false,
            );
            return Ok(());
        }
    };
    println!(
        "  Settings ▸ Harness in window {} (view {}); the legs' window {}, in front",
        desk.page_window, desk.page_view, desk.chains_window
    );
    if b.tells_hold {
        model::hold_worker_land();
    }
    let manager = proof
        .inst
        .ctl(&["spawn", &format!("cwd={}", proof.s.display())])
        .first()
        .strip_prefix("OK ")
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string);
    let Some(manager) = manager else {
        proof.check("window: the manager's tab is spawned", false);
        return Ok(());
    };
    let _ = proof.on(&manager, &["meta", "set", "title", "manager"]);
    let land = proof.open_leg("land")?;
    let reland = proof.open_leg("reland")?;
    let off = proof.open_leg("off")?;
    let worker = proof.spawn_worker(Some(&manager))?;
    let mut legs: Vec<(Scene, Leg)> = vec![
        (Scene::Land, land),
        (Scene::Reland, reland),
        (Scene::Off, off),
    ];
    if let Some(worker) = worker {
        legs.push((Scene::Worker, worker));
    }
    let ls = proof.inst.ctl(&["ls"]).stdout;
    for (scene, leg) in &legs {
        let titled = proof.on(&leg.sid, &["meta", "set", "title", leg.name]);
        if !titled.first().starts_with("OK") {
            proof.check(
                &format!("window: {} titled ({})", leg.name, titled.first()),
                false,
            );
        }
        let w = surfaces::window_of(&ls, &leg.sid).unwrap_or_default();
        if *scene != Scene::Worker && w != desk.chains_window {
            proof.check(
                &format!(
                    "window: {} in the legs' window {} (it is in {w:?}), where the strip's hover \
                     is read",
                    leg.name, desk.chains_window
                ),
                false,
            );
        }
    }
    for (_, leg) in &legs {
        proof.claim(leg)?;
    }
    if let Some((_, off)) = legs.iter().find(|(s, _)| *s == Scene::Off) {
        proof.autopilot_off(off)?;
    }
    let seen = read_until_rest(proof, &legs, &desk);
    model::release_worker_land();
    let seen = seen?;
    for ((scene, leg), s) in legs.iter().zip(&seen) {
        judge(proof, &b, *scene, leg, s);
    }
    idle(proof, &legs, "Settings ▸ Harness open", &[PAGE_TURN])?;
    match shut_page(proof, &desk) {
        Ok(said) => {
            println!("  {said}");
            idle(proof, &legs, "Settings ▸ Harness shut", &[])?;
        }
        Err(why) => {
            proof.check(
                &format!("window: Settings ▸ Harness shut for END's second idle window ({why})"),
                false,
            );
        }
    }
    Ok(())
}

/// Every leg read each [`POLL`] until all are at rest ([`at_rest`]) or [`HANG`] ran out. The
/// worker's held land act is let go once its row read `landing`.
fn read_until_rest(
    proof: &Proof,
    legs: &[(Scene, Leg)],
    desk: &Desk,
) -> Result<Vec<Seen>, Stopped> {
    let t0 = crate::util::now_ms();
    let mut seen: Vec<Seen> = legs.iter().map(|_| Seen::default()).collect();
    let end = Instant::now() + HANG;
    loop {
        let chrome = proof.inst.ctl(&["chrome"]).stdout;
        let page = proof
            .inst
            .ctl(&["inspect", "app/v1", "view", &desk.page_view, "text"])
            .stdout;
        let g = Glance::of(crate::util::now_ms(), &chrome, &page);
        let t = g.t - t0;
        let here = g
            .menu
            .iter()
            .find(|r| r.section == "here")
            .map_or_else(|| "-".to_string(), |r| r.label.clone());
        for ((scene, leg), s) in legs.iter().zip(seen.iter_mut()) {
            let status = proof.status(&leg.sid);
            let words = [
                "autopilot",
                "task",
                "land_held",
                "land_since",
                "supervisor_proven",
                "end",
            ]
            .iter()
            .map(|k| format!("{k}={}", field(&status, k).unwrap_or_else(|| "?".into())))
            .collect::<Vec<_>>()
            .join(" ");
            let read = Read {
                line: surfaces::chip_of(&g.chrome, &leg.sid).unwrap_or_default(),
                words,
                pad: pad_of(&proof.on(&leg.sid, &["text"]).stdout),
                state: if g.page_read {
                    state_of(&g.page, leg.name)
                } else {
                    "?".into()
                },
                hover: if g.hover_read {
                    g.strip
                        .iter()
                        .find(|tab| tab.label == leg.name)
                        .and_then(StripTab::driven)
                        .unwrap_or_else(|| "-".into())
                } else {
                    "?".into()
                },
                here: here.clone(),
            };
            if s.reads.last().is_none_or(|(_, r)| *r != read) {
                println!(
                    "    {} {}: {} | {} | pad {:?} | state {} | hover {} | here {}",
                    secs_at(t),
                    leg.name,
                    read.line,
                    read.words,
                    read.pad,
                    read.state,
                    read.hover,
                    read.here
                );
                s.reads.push((t, read));
            }
            s.last_t = t;
            push_new(&mut s.rows, t, surfaces::menu_of(&g.menu, leg.name));
            let under = surfaces::worker_of(&g.sub, leg.name);
            if *scene == Scene::Worker && under_says(&under, "landing") {
                model::release_worker_land();
            }
            push_new(&mut s.under, t, under);
        }
        let now = crate::util::now_ms() - t0;
        let reached = legs
            .iter()
            .zip(&seen)
            .all(|((scene, leg), s)| at_rest(proof, *scene, leg, s, now));
        if reached || Instant::now() >= end {
            return Ok(seen);
        }
        sleep(POLL)?;
    }
}

fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// The trailing run of `s`'s reads that show the task landed, oldest first.
fn landed_run(s: &Seen) -> &[(i64, Read)] {
    let from = s
        .reads
        .iter()
        .rposition(|(_, r)| !r.landed())
        .map_or(0, |i| i + 1);
    &s.reads[from..]
}

/// Whether `leg` reached the end of its scene, where its window stands still, at `now` (ms from
/// the pass's start).
fn at_rest(proof: &Proof, scene: Scene, leg: &Leg, s: &Seen, now: i64) -> bool {
    let Some((_, last)) = s.reads.last() else {
        return false;
    };
    match scene {
        Scene::Land | Scene::Reland => landing_at_rest(s, leg.name, now),
        Scene::Off => {
            last.word("supervisor_proven").as_deref() != Some("1")
                && proof
                    .on(&leg.sid, &["text"])
                    .stdout
                    .contains(model::STEP_ONE)
        }
        Scene::Worker => {
            s.under.last().is_some_and(|(_, u)| under_says(u, "landed"))
                || last.landed() && s.under.iter().all(|(_, u)| u == "-")
        }
    }
}

/// Whether a landing leg labelled `label` is at rest at `now`: once its Done for you row is read;
/// or once a read past the landing still says `end=-` (no end told, which is judged); or
/// [`ROW_HANG`] after `end=` told the end (or after the landing, on an aterm whose `status`
/// prints no `end=`) with no row. Never on elapsed time alone while the row is still owed.
fn landing_at_rest(s: &Seen, label: &str, now: i64) -> bool {
    let Some((_, last)) = s.reads.last() else {
        return false;
    };
    let run = landed_run(s);
    let Some((landed_at, _)) = run.first() else {
        return false;
    };
    if done_row_seen(s, label).is_some() {
        return true;
    }
    match last.word("end").as_deref() {
        Some("-") => s.last_t > *landed_at,
        Some("?") | None => now.saturating_sub(*landed_at) >= ms(ROW_HANG),
        Some(_) => run
            .iter()
            .find(|(_, r)| r.word("end").is_some_and(|e| e != "-" && e != "?"))
            .is_some_and(|(told, _)| now.saturating_sub(*told) >= ms(ROW_HANG)),
    }
}

fn done_row_seen<'s>(s: &'s Seen, label: &str) -> Option<&'s (i64, String)> {
    let want = format!("\u{2713} {label} \u{2014} landed");
    s.rows
        .iter()
        .find(|(_, r)| r.lines().any(|l| l.contains(&want)))
}

/// A time from the pass's start, in whole seconds.
fn secs_at(t: i64) -> String {
    format!("+{} s", t / 1000)
}

/// The first read of `s` where `want` holds, with its time.
fn first_read(s: &Seen, want: impl Fn(&Read) -> bool) -> Option<&(i64, Read)> {
    s.reads.iter().find(|(_, r)| want(r))
}

/// The scene's wants, each a check. A landed face is judged on the leg's last read, at rest: each
/// poll reads `chrome` once before every leg's `status`, so the read where `task=landed` first
/// shows may hold the chrome from just before the landing.
fn judge(proof: &mut Proof, b: &Build, scene: Scene, leg: &Leg, s: &Seen) {
    let name = leg.name;
    let landing = |r: &Read| {
        r.word("autopilot").as_deref() == Some("land") && r.word("task").as_deref() == Some("open")
    };
    let face_check = |proof: &mut Proof, what: &str, at: Option<&(i64, Read)>, face: &str| {
        let (held, said) = at.map_or_else(
            || (false, format!("never read ({} reads)", s.reads.len())),
            |(t, r)| match judge_line(&r.line, r.pad.as_deref(), face, b.says_mode) {
                Ok(said) => (true, format!("{}: {said}", secs_at(*t))),
                Err(why) => (false, format!("{}: {why}", secs_at(*t))),
            },
        );
        proof.check(&format!("window: {name} {what}: {said}"), held);
    };
    match scene {
        Scene::Land => {
            face_check(
                proof,
                "under Land, before its landing, the chip line says face=land",
                first_read(s, |r| landing(r) && !r.line.is_empty()),
                "land",
            );
            hold_checks(proof, b, name, s);
            face_check(
                proof,
                "once landed, the chip line says face=landed",
                s.reads.last().filter(|(_, r)| r.landed()),
                "landed",
            );
            end_check(proof, s, name);
            done_check(proof, s, name);
        }
        Scene::Reland => {
            face_check(
                proof,
                "once landed, the chip line says face=landed",
                s.reads.last().filter(|(_, r)| r.landed()),
                "landed",
            );
            end_check(proof, s, name);
            done_check(proof, s, name);
        }
        Scene::Off => {
            let released = first_read(s, |r| {
                r.word("supervisor_proven").as_deref() != Some("1")
                    && r.word("autopilot").as_deref() == Some("off")
            });
            let (said, held) = released.map_or_else(
                || ("never read with its claim given back".to_string(), false),
                |(t, r)| match judge_line(&r.line, r.pad.as_deref(), "off", b.says_mode) {
                    Ok(said) => (format!("{}: {said}", secs_at(*t)), true),
                    Err(why) => (format!("{}: {why}", secs_at(*t)), false),
                },
            );
            proof.check(
                &format!(
                    "window: {name} under Autopilot Off, its claim given back, no chip: {said}"
                ),
                held,
            );
        }
        Scene::Worker => {
            let said = s
                .under
                .iter()
                .map(|(t, u)| format!("{} {}", secs_at(*t), u.replace('\n', " ")))
                .collect::<Vec<_>>()
                .join("; ");
            let land_act = proof
                .tags(name)
                .iter()
                .any(|t| t.starts_with("worker:land"));
            match worker_rows(&s.under, b.tells_hold && land_act) {
                Ok(order) => {
                    proof.check(
                        &format!("window: {name} its row under its manager {order} ({said})"),
                        true,
                    );
                }
                Err(Some(why)) => {
                    proof.check(
                        &format!("window: {name} its row under its manager: {why} ({said})"),
                        false,
                    );
                }
                Err(None) => {
                    proof.check(
                        &format!("window: {name} its row under its manager reads landed ({said})"),
                        true,
                    );
                    let why = if b.tells_hold {
                        "the model's log holds no land act of the worker's".to_string()
                    } else {
                        format!(
                            "codex-alab {} tells no Land hold (that came with build \
                             {TELLS_LAND_HOLD}, §14.3.3), and a row says landing only while a \
                             hold stands (§12.4.3)",
                            b.version
                        )
                    };
                    println!("  skip window: {name} its row's `landing`: {why}");
                }
            }
        }
    }
}

/// The Land leg's hold on every surface it reaches (§14.3.3), for a build that tells it: told on
/// `status`, said on the state line and This Mac's `landing` count while it stands, on the hover
/// once a yield was judged, and gone from all three once landed. For an older build, one named
/// skip.
fn hold_checks(proof: &mut Proof, b: &Build, name: &str, s: &Seen) {
    if !b.tells_hold {
        println!(
            "  skip window: {name} its hold's status, state line, hover and This Mac count: \
             codex-alab {} tells no Land hold (that came with build {TELLS_LAND_HOLD}, §14.3.3)",
            b.version
        );
        return;
    }
    let said = |at: Option<&(i64, Read)>, what: &dyn Fn(&Read) -> String| {
        at.map_or_else(
            || "never read".to_string(),
            |(t, r)| format!("{}: {}", secs_at(*t), what(r)),
        )
    };
    let told = first_read(s, Read::held);
    proof.check(
        &format!(
            "window: {name} its hold told by codex-alab reads on the tab's status ({})",
            said(told, &|r: &Read| r.words.clone())
        ),
        told.is_some(),
    );
    let line = first_read(s, |r| {
        r.held() && r.state.contains("codex-alab") && r.state.contains("landing since")
    });
    proof.check(
        &format!(
            "window: {name} its state line says codex-alab's hold, `landing since` ({})",
            said(line, &|r: &Read| r.state.clone())
        ),
        line.is_some(),
    );
    let here = first_read(s, |r| {
        r.held() && surfaces::count_in(&r.here, "landing") >= 1
    });
    proof.check(
        &format!(
            "window: {name} This Mac counts it landing while it holds ({})",
            said(here, &|r: &Read| r.here.clone())
        ),
        here.is_some(),
    );
    let hover = first_read(s, |r| {
        r.judged() && r.hover.contains("codex-alab: not landed yet")
    });
    let judged = first_read(s, Read::judged);
    proof.check(
        &format!(
            "window: {name} its hover says codex-alab's judged hold ({})",
            match (hover, judged) {
                (Some(_), _) => said(hover, &|r: &Read| r.hover.clone()),
                (None, Some((t, r))) => format!(
                    "{}: {} with a yield judged ({})",
                    secs_at(*t),
                    r.hover,
                    r.words
                ),
                (None, None) => "no status read showed a judged yield (land_held=)".into(),
            }
        ),
        hover.is_some(),
    );
    let gone = s.reads.last().filter(|(_, r)| r.landed());
    let left = gone.map(|(_, r)| {
        let mut left = Vec::new();
        if r.state == "?" || r.state.contains("landing since") || r.state.contains("not landed yet")
        {
            left.push(format!("state {:?}", r.state));
        }
        if r.hover == "?" || r.hover.contains("not landed yet") {
            left.push(format!("hover {:?}", r.hover));
        }
        if surfaces::count_in(&r.here, "landing") != 0 {
            left.push(format!("This Mac {:?}", r.here));
        }
        left
    });
    proof.check(
        &format!(
            "window: {name} once landed its hold is gone from the state line, hover and This Mac \
             ({})",
            match (&left, gone) {
                (Some(left), Some((t, r))) if left.is_empty() => format!(
                    "{}: state {:?}, hover {:?}, here {:?}",
                    secs_at(*t),
                    r.state,
                    r.hover,
                    r.here
                ),
                (Some(left), Some((t, _))) => format!("{}: still {}", secs_at(*t), left.join(", ")),
                _ => "never read landed".into(),
            }
        ),
        left.is_some_and(|l| l.is_empty()),
    );
}

/// A worker's rows under its manager, judged: Ok with the order read where its hold was told
/// (`held`: its build tells it and its land act was typed) and its row went from landing to
/// landed; `Err(None)` where none was told and the row reads landed (the skip names why);
/// `Err(Some(why))` otherwise. A worker under the work floor (§11.2.3) wears no Done for you row,
/// so none is wanted of it.
fn worker_rows(under: &[(i64, String)], held: bool) -> Result<&'static str, Option<String>> {
    let landing = under.iter().find(|(_, u)| under_says(u, "landing"));
    let landed = under.iter().find(|(_, u)| under_says(u, "landed"));
    match (landing, landed, held) {
        (Some((a, _)), Some((b, _)), _) if a <= b => Ok("goes from landing to landed"),
        (Some(_), Some(_), _) => Err(Some("landed before it read landing".into())),
        (None, Some(_), false) => Err(None),
        (None, Some(_), true) => Err(Some(
            "its hold was told (its land act typed, held until its row read landing), yet its \
             row never read landing"
                .into(),
        )),
        (_, None, _) => Err(Some("its row never read landed".into())),
    }
}

/// A landing past the work floor is an end: `status end=` names when it was told. An aterm whose
/// `status` prints no `end=` (a release build) is a named skip.
fn end_check(proof: &mut Proof, s: &Seen, name: &str) {
    let last = s.reads.last().filter(|(_, r)| r.landed());
    match last.map(|(t, r)| (t, r.word("end"))) {
        Some((_, Some(e))) if e == "?" => println!(
            "  skip window: {name} its landing is an end: this aterm's status prints no end= (a \
             release build)"
        ),
        Some((t, e)) => {
            let e = e.unwrap_or_else(|| "?".into());
            proof.check(
                &format!(
                    "window: {name} its landing is an end ({}: status end={e})",
                    secs_at(*t)
                ),
                e != "-",
            );
        }
        None => {
            proof.check(
                &format!("window: {name} its landing is an end (never read landed)"),
                false,
            );
        }
    }
}

fn done_check(proof: &mut Proof, s: &Seen, name: &str) {
    let row = done_row_seen(s, name);
    proof.check(
        &format!(
            "window: {name} a Done for you row once landed ({})",
            row.map_or_else(
                || "none in the menu bar".to_string(),
                |(t, r)| format!("{}: {}", secs_at(*t), r.replace('\n', " ")),
            )
        ),
        row.is_some(),
    );
}

/// The tab index of `view`, Settings ▸ Harness, in `window` (`inspect app/v1 tabs`).
fn tab_of_view(tabs: &str, window: &str, view: &str) -> Option<String> {
    let (mut here, mut tab) = (false, None);
    for l in tabs.lines() {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("window ") {
            here = crate::forever::word(rest, "id").as_deref() == Some(window);
        } else if t.starts_with("tab ") {
            tab = crate::forever::word(t, "index");
        } else if here
            && t.starts_with("leaf ")
            && crate::forever::word(t, "view").as_deref() == Some(view)
        {
            return tab;
        }
    }
    None
}

/// Settings ▸ Harness's tab closed in its window, by a session that window hosts (`@<sid> tab
/// close <i>`): what was done, or why it is still open.
fn shut_page(proof: &Proof, desk: &Desk) -> Result<String, String> {
    let tabs = || proof.inst.ctl(&["inspect", "app/v1", "tabs"]).stdout;
    let index = tab_of_view(&tabs(), &desk.page_window, &desk.page_view).ok_or_else(|| {
        format!(
            "no tab holds view {} in window {}",
            desk.page_view, desk.page_window
        )
    })?;
    let ls = proof.inst.ctl(&["ls"]).stdout;
    let sid = ls
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2))
        .find(|sid| surfaces::window_of(&ls, sid).as_deref() == Some(desk.page_window.as_str()))
        .ok_or_else(|| {
            format!(
                "no session in window {} to close its tab by",
                desk.page_window
            )
        })?;
    let closed = proof
        .inst
        .ctl(&[&format!("@{sid}"), "tab", "close", &index])
        .first();
    match surfaces::windows_of(&tabs()).1 {
        None => Ok(format!(
            "Settings ▸ Harness shut: tab {index} of window {} closed ({closed})",
            desk.page_window
        )),
        Some((w, v)) => Err(format!(
            "`@{sid} tab close {index}` said {closed:?}, and window {w} still shows Settings \
             (view {v})"
        )),
    }
}

/// END: once every leg's journal stood still [`QUIET`], one idle window (`what`: the page open,
/// or shut): `metrics reset`, its settle, the opening counts, [`surfaces::IDLE_MS`] with no read,
/// the closing counts, judged by [`surfaces::idle_counts_beside`] with the owners `also` that may
/// arm there, never fire ([`PAGE_TURN`] while the page is open).
fn idle(
    proof: &mut Proof,
    legs: &[(Scene, Leg)],
    what: &str,
    also: &[&str],
) -> Result<(), Stopped> {
    let counts = |proof: &Proof| -> Vec<usize> {
        legs.iter().map(|(_, l)| proof.acts(&l.sid).len()).collect()
    };
    let (mut acts, mut since) = (counts(proof), Instant::now());
    let end = Instant::now() + HANG;
    let quiet = loop {
        if since.elapsed() >= QUIET {
            break true;
        }
        if Instant::now() >= end {
            break false;
        }
        sleep(Duration::from_secs(1))?;
        let now = counts(proof);
        if now != acts {
            (acts, since) = (now, Instant::now());
        }
    };
    let read = |args: &[&str]| proof.inst.ctl(args);
    let _ = read(&["metrics", "reset"]);
    sleep(Duration::from_secs(2))?;
    let (m0, p0) = (
        read(&["metrics"]).stdout,
        read(&["metrics", "percentiles"]).stdout,
    );
    sleep(Duration::from_millis(
        u64::try_from(surfaces::IDLE_MS).unwrap_or(20_000),
    ))?;
    let (m1, p1) = (
        read(&["metrics"]).stdout,
        read(&["metrics", "percentiles"]).stdout,
    );
    let m = surfaces::idle_metrics_between(&m0, &m1);
    let p = surfaces::idle_percentiles_between(&p0, &p1);
    let bad = surfaces::idle_counts_beside(&m, &p, also);
    let said = [
        "timer_wakes",
        "deadline_arms_by_owner",
        "deadline_owner",
        "deadline_in_ms",
        "wakes_by_variant",
    ]
    .iter()
    .map(|k| {
        format!(
            "{k}={}",
            crate::forever::word(&m, k).unwrap_or_else(|| "-".into())
        )
    })
    .chain(std::iter::once(format!(
        "n_render={}",
        crate::forever::word(&p, "n_render").unwrap_or_else(|| "-".into())
    )))
    .collect::<Vec<_>>()
    .join(" ");
    proof.check(
        &format!(
            "window: END idle {} s, {what}, with every leg at rest{}: {said}{}",
            surfaces::IDLE_MS / 1000,
            if quiet {
                ""
            } else {
                " (its acts never stood still)"
            },
            if bad.is_empty() {
                String::new()
            } else {
                format!("; {}", bad.join("; "))
            }
        ),
        quiet && bad.is_empty(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAND_NOT_DRAWN: &str =
        "window=0 program=codex face=land drawn=- why=no-footer spoken=- description=- title=-";
    const LAND_DRAWN: &str = "window=0 program=codex face=land drawn=\"\u{2913} land\" spoken=\"Autopilot: land.\" description=- title=-";
    const OFF_SWITCH: &str =
        "window=0 program=codex face=off drawn=- why=no-footer spoken=- description=- title=-";
    /// 14249's line under Land (`window-branch-14249-final.log`).
    const OWN: &str = "codex-alab 0.1.0-dev.14249 \u{b7} based on OpenAI Codex 0.166.0 \
                       \u{2500}\u{2500} \u{2913} land \u{2500}[Cruise]\u{2500}[Land]\u{2500}[Off]\
                       \u{2500}\u{2500}\u{2500}[ALab on]";
    /// 13916's line under Land, landed alike (`window-13916-try1.log`): the mode's button lit,
    /// no word.
    const OWN_13916: &str = "codex-alab 0.1.0-dev.13916 \u{b7} based on OpenAI Codex 0.165.0 \
                             \u{2500}\u{2500}\u{2500}[Cruise]\u{2500}[Land]\u{2500}[Off]\u{2500}\
                             \u{2500}\u{2500}[ALab on]";

    #[test]
    fn the_pad_is_the_row_over_the_last_prompt() {
        let text = format!("> hello\n\n  {OWN}  \n\u{203a} Ask Codex\n\n  ? for shortcuts\n");
        assert_eq!(pad_of(&text).as_deref(), Some(OWN));
        assert_eq!(
            pad_of("> hello\n\n\u{203a} Ask Codex\n").as_deref(),
            Some(""),
            "a blank pad"
        );
        assert_eq!(pad_of("no prompt here\n"), None, "CONTROL: no prompt row");
    }

    #[test]
    fn a_build_says_what_it_tells() {
        assert_eq!(
            dev_build("0.1.0-dev.14249+codex.0.166.0.g76c628303"),
            Some(14_249)
        );
        assert_eq!(dev_build("0.1.0-dev.13916"), Some(13_916));
        assert_eq!(dev_build("0.164.0"), None, "CONTROL: no dev build");
        let b = Build::of("0.1.0-dev.14249+codex.0.166.0.g76c628303".into());
        assert!(b.tells_hold && b.says_mode);
        let b = Build::of("0.1.0-dev.14157".into());
        assert!(b.tells_hold && !b.says_mode, "the hold before the word");
        let b = Build::of("0.1.0-dev.13916+codex.0.165.0.gabc".into());
        assert!(!b.tells_hold && !b.says_mode, "the owner's installed build");
        assert!(
            !Build::of("?".into()).tells_hold,
            "CONTROL: no version read"
        );
    }

    /// THE CHIP NEVER COVERS THE PROGRAM'S OWN LINE: a chip drawn while the pad holds
    /// codex-alab's own text fails whatever its face. A chip on a blank pad fails too.
    #[test]
    fn a_chip_is_judged_against_the_programs_own_pad() {
        assert!(judge_line(LAND_NOT_DRAWN, Some(OWN), "land", true).is_ok());
        assert!(
            judge_line(
                &LAND_NOT_DRAWN.replace("face=land", "face=-"),
                Some(OWN),
                "land",
                true
            )
            .is_ok(),
            "a native mode needs no host-rendered face"
        );
        assert!(judge_line(LAND_DRAWN, Some(""), "land", true).is_err());
        let covered = judge_line(LAND_DRAWN, Some(OWN), "land", true).expect_err("covered");
        assert!(covered.contains("aterm drew Codex controls"), "{covered}");
        assert!(
            judge_line(LAND_NOT_DRAWN, Some(""), "land", true).is_err(),
            "CONTROL: no chip and nothing of the program's on a blank pad"
        );
        assert!(
            judge_line(LAND_DRAWN, Some(""), "landed", true).is_err(),
            "CONTROL: the wrong face"
        );
        assert!(
            judge_line(
                &LAND_DRAWN.replace("\u{2913} land", "\u{221e}"),
                Some(""),
                "land",
                true
            )
            .is_err(),
            "CONTROL: drawn cells that are not its face"
        );
        assert!(judge_line(OFF_SWITCH, Some(""), "off", true).is_err());
        let off = OWN.replace("\u{2913} land", "\u{25cb} off");
        assert!(judge_line(OFF_SWITCH, Some(&off), "off", true).is_ok());
        assert!(
            judge_line(
                "window=0 program=codex face=off drawn=\"\u{25cb} off\" spoken=- description=-",
                Some(""),
                "off",
                true
            )
            .is_err(),
            "CONTROL: a chip where none belongs"
        );
        assert!(judge_line("", Some(OWN), "land", true).is_err(), "no line");
    }

    /// A BUILD THAT NAMES ITS MODE MUST NAME THE FACE: beside codex-alab's own line, a build from
    /// [`SAYS_ITS_MODE`] on fails where its line lacks the face's words; 13916's line, which
    /// lights a button and names no mode, passes only for a build before it.
    #[test]
    fn its_own_line_names_the_face_from_the_build_that_says_it() {
        let landed = LAND_NOT_DRAWN.replace("face=land", "face=landed");
        let said = OWN.replace("\u{2913} land", "\u{2713} landed");
        assert!(judge_line(&landed, Some(&said), "landed", true).is_ok());
        let stale = judge_line(&landed, Some(OWN), "landed", true).expect_err("stale");
        assert!(stale.contains("names no landed"), "{stale}");
        assert!(
            judge_line(&landed, Some(OWN_13916), "landed", true).is_err(),
            "CONTROL: 13916's line under a build that should say it"
        );
        let old = judge_line(&landed, Some(OWN_13916), "landed", false).expect("old");
        assert!(old.contains("no host footer"), "{old}");
    }

    #[test]
    fn a_workers_rows_are_judged_by_the_hold_it_was_told() {
        let row = |w: &str| {
            format!("mgr|manager \u{2014} 1 worker: 1 {w}\nrow|\u{25cf} worker \u{2014} {w}")
        };
        let at = |t: i64, w: &str| (t, row(w));
        let went = [at(0, "working"), at(2, "landing"), at(6, "landed")];
        assert_eq!(worker_rows(&went, true), Ok("goes from landing to landed"));
        let skipped = [at(0, "working"), at(6, "landed")];
        assert_eq!(
            worker_rows(&skipped, false),
            Err(None),
            "no hold told: a named skip"
        );
        assert!(
            matches!(worker_rows(&skipped, true), Err(Some(_))),
            "CONTROL: a hold told and no landing row"
        );
        assert!(
            matches!(
                worker_rows(&[at(0, "landed"), at(2, "landing")], true),
                Err(Some(_))
            ),
            "CONTROL: landed before landing"
        );
        assert!(
            matches!(worker_rows(&[at(0, "working")], false), Err(Some(_))),
            "CONTROL: never landed"
        );
    }

    #[test]
    fn a_workers_row_says_landing_then_landed() {
        let landing =
            "mgr|manager \u{2014} 1 worker: landing\nrow|\u{25cf} worker \u{2014} landing";
        let landed = "mgr|manager \u{2014} 1 worker: landed\nrow|\u{2713} worker \u{2014} landed #";
        assert!(under_says(landing, "landing") && !under_says(landing, "landed"));
        assert!(under_says(landed, "landed") && !under_says(landed, "landing"));
        assert!(!under_says("-", "landed"), "CONTROL: no row");
    }

    fn read(t: i64, words: &str) -> (i64, Read) {
        (
            t,
            Read {
                line: String::new(),
                words: words.into(),
                pad: None,
                state: "-".into(),
                hover: "-".into(),
                here: "-".into(),
            },
        )
    }

    /// A LANDING RESTS ON WHAT IT TOLD, NOT ON A CLOCK: its Done for you row is waited for up to
    /// [`ROW_HANG`] after `end=` told the end; a landing a later read still shows with `end=-` is
    /// at rest at once (its missing end is judged); a read of the landing alone is not; its row
    /// read rests it. CONTROL: before [`ROW_HANG`] with the end told and no row, it is not at rest.
    #[test]
    fn a_landing_rests_on_its_end_and_its_row() {
        let landed = |end: &str| format!("task=landed end={end}");
        let mut s = Seen {
            reads: vec![read(0, "task=open end=-"), read(10_000, &landed("-"))],
            last_t: 10_000,
            ..Seen::default()
        };
        assert!(
            !landing_at_rest(&s, "land", 10_000),
            "the landing's first read alone"
        );
        s.last_t = 12_000;
        assert!(
            landing_at_rest(&s, "land", 12_000),
            "a later read still says end=-"
        );
        s.reads[1] = read(10_000, &landed("1793451264"));
        assert!(
            !landing_at_rest(&s, "land", 10_000 + ms(ROW_HANG) - 1),
            "CONTROL: the end told, its row not yet due"
        );
        assert!(
            landing_at_rest(&s, "land", 10_000 + ms(ROW_HANG)),
            "the row's hang detector"
        );
        s.rows
            .push((11_000, "done|\u{2713} land \u{2014} landed 1m".to_string()));
        assert!(landing_at_rest(&s, "land", 11_000), "its row read");
        assert!(
            !landing_at_rest(&s, "reland", 11_000),
            "CONTROL: another tab's row"
        );
        assert_eq!(landed_run(&s).len(), 1);
    }

    /// THE OPEN PAGE'S NEXT TURN MAY ARM, NEVER FIRE: with Settings ▸ Harness open and every leg
    /// at rest, the page's next turn of its words ([`PAGE_TURN`], hours away) is the loop's only
    /// deadline and arms at each wake (this tree's END, page open: `autopilot_timer:14/0`); END
    /// passes it there, and only there. CONTROL: the page shut, the same arms fail; a fire of it
    /// fails either way.
    #[test]
    fn the_open_pages_next_turn_may_arm_in_ends_idle_but_never_fire() {
        const OPEN: &str = "timer_wakes=0 event_wakes=17 deadline_owner=autopilot_timer \
                            deadline_arms_by_owner=autopilot_timer:14/0 deadline_fires_by_owner=none \
                            wakes_by_variant=ReadSessionStatuses:11,ReadSessionStatus:4,HostNote:1";
        const FIRED: &str = "timer_wakes=1 event_wakes=17 deadline_owner=autopilot_timer \
                             deadline_arms_by_owner=autopilot_timer:14/0 \
                             deadline_fires_by_owner=autopilot_timer:1 \
                             wakes_by_variant=ReadSessionStatuses:11,ReadSessionStatus:4";
        let p = "n_render=0";
        assert_eq!(
            surfaces::idle_counts_beside(OPEN, p, &[PAGE_TURN]),
            Vec::<String>::new()
        );
        assert!(
            !surfaces::idle_counts_beside(OPEN, p, &[]).is_empty(),
            "CONTROL: the page shut"
        );
        assert!(
            !surfaces::idle_counts_beside(FIRED, p, &[PAGE_TURN]).is_empty(),
            "CONTROL: a fire in the window"
        );
    }

    #[test]
    fn the_state_line_and_the_pages_tab_are_found() {
        let rows = vec![
            (
                "harness/session/7".to_string(),
                "land: Window 2, tab 1".to_string(),
            ),
            (
                "harness/session/7/state".to_string(),
                "codex-alab: landing since 09:12".to_string(),
            ),
            (
                "harness/session/all".to_string(),
                "Every session".to_string(),
            ),
        ];
        assert_eq!(state_of(&rows, "land"), "codex-alab: landing since 09:12");
        assert_eq!(state_of(&rows, "reland"), "-", "CONTROL: no row for it");
        let tabs = "window id=1\n  tab index=0 active=false\n    leaf kind=term sid=s-a\n  \
                    tab index=1 active=true\n    leaf kind=app app=settings view=9\nwindow id=2\n  \
                    tab index=0 active=true\n    leaf kind=term sid=s-b\n";
        assert_eq!(tab_of_view(tabs, "1", "9").as_deref(), Some("1"));
        assert_eq!(tab_of_view(tabs, "2", "9"), None, "CONTROL: another window");
    }
}
