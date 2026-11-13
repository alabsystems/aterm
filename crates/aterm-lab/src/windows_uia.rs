// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `windows uia`: what a UI Automation client sees of a windowed aterm-gui in the lane's desktop,
//! held to a committed record per arch (WIN-UIA in docs/AUDIT-third-party-2026-10-15.md).
//!
//! The client is .NET's UIAutomationClient, UIA's client API over UIAutomationCore, run by the desk
//! helper ([`desk::HELPER`]'s `uia-watch`, `uia-act` and `uia-tree`). It watches the window's
//! events while the lane types a line, takes the focus away and gives it back, opens a second tab
//! and selects the first again through its tab item's pattern, and opens the find bar on the echo; then
//! it walks the window's raw view and reads every pattern. What it saw is a [`Recording`]: the events of each phase as the set of
//! distinct lines, every element's properties and patterns, and the text reads.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::linux_baseline::{Recording, compare, render_diff};
use crate::paths::paths;
use crate::windows::{self, Up, not_run, run_on_desktop, task_name};
use crate::windows_desk::{self as desk, ATERM_CLASS, helper_line};
use crate::windows_guest::{self as guest, Arch, Desktop};
use crate::windows_window::{
    Display, Session, click, cpu_config, desk as run_desk, display_detail, put_helper, set_display,
    type_text, within,
};
use crate::{Fail, LAB, Res};

/// Where the committed records live, `<arch>.txt` each.
pub const DIR: &str = "crates/aterm-lab/windows-uia";

/// What the lane types, and the echo it waits for.
pub const TYPED: &str = "Write-Output ('UIA-' + 6*7)\n";
pub const ECHO: &str = "UIA-42";

/// What the second tab's shell shows once it is up.
const PROMPT: &str = "PS C:";

/// The phases whose events are compared, in the order they run. Before them, `start` is the
/// provider's activation as the watcher subscribes, which races the subscription and is measured
/// only.
pub const PHASES: [&str; 5] = ["type", "blur", "focus", "tab", "find"];

/// The session's instance number ([`desk::instance_dir`]): the window smoke's are 1 and 2.
const INSTANCE: u32 = 3;

/// How long the events of one action are given to arrive before the next phase.
const SETTLE: Duration = Duration::from_secs(3);

/// The record of `events` (the watcher's log) and `tree` (`uia-tree`'s walk) for `arch`.
#[must_use]
pub fn compose(arch: Arch, events: &str, tree: &str) -> Recording {
    let mut r = Recording {
        comments: vec![
            format!(
                "aterm Windows lane UIA record (WIN-UIA): {} ({}), a windowed aterm-gui as .NET's \
                 UIAutomationClient sees it",
                arch.name(),
                arch.runs()
            ),
            "= compared exactly by the replay; ~ measured, shown when it moves".into(),
            format!(
                "replay: {LAB} windows uia --arch {} (add --record to rewrite this file)",
                arch.name()
            ),
        ],
        lines: Vec::new(),
    };
    let mut seen: BTreeMap<&str, BTreeMap<&str, u32>> = BTreeMap::new();
    for line in events.lines().map(str::trim_end) {
        if let Some((phase, what)) = line.split_once(' ') {
            *seen.entry(phase).or_default().entry(what).or_default() += 1;
        }
    }
    let set = |phase: &str| {
        seen.get(phase).map_or_else(
            || "none".to_string(),
            |m| m.keys().copied().collect::<Vec<_>>().join(" | "),
        )
    };
    let counts = |phase: &str| {
        seen.get(phase).map_or_else(
            || "none".to_string(),
            |m| {
                m.iter()
                    .map(|(what, n)| format!("{n}x {what}"))
                    .collect::<Vec<_>>()
                    .join(" | ")
            },
        )
    };
    for phase in PHASES {
        r.behaviour(&format!("event.{phase}"), set(phase));
    }
    r.measured("event.start", set("start"));
    for phase in std::iter::once("start").chain(PHASES) {
        r.measured(&format!("event.{phase}.count"), counts(phase));
    }
    for line in tree.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        let mut words = line.splitn(3, ' ');
        match (words.next(), words.next(), words.next()) {
            (Some(kind @ ("tree" | "pattern")), Some(at), rest) => {
                r.behaviour(&format!("{kind}.{at}"), rest.unwrap_or_default());
            }
            _ => r.behaviour("walk.other", line),
        }
    }
    r
}

/// The elements and the text reads a record holds, for its row.
fn summary(r: &Recording) -> String {
    let count = |p: &str| r.lines.iter().filter(|l| l.key.starts_with(p)).count();
    let events: BTreeSet<&str> = PHASES
        .iter()
        .filter_map(|phase| {
            r.lines
                .iter()
                .find(|l| l.key == format!("event.{phase}"))
                .map(|l| l.value.as_str())
        })
        .flat_map(|v| v.split(" | "))
        .filter(|v| *v != "none")
        .collect();
    format!(
        "{} elements, {} pattern reads, {} distinct events over {}",
        count("tree."),
        count("pattern."),
        events.len(),
        PHASES.join(", ")
    )
}

/// Write `stage` to the watcher's phase file.
fn set_phase(up: &Up, file: &str, stage: &str) -> Result<(), String> {
    let out = up.ssh.run(&format!("echo {stage}> {file}"));
    if out.ok() {
        Ok(())
    } else {
        Err(format!("the phase file refused {stage}: {}", out.tail(2)))
    }
}

/// Copy the guest's `remote` (backslashes) to `results/name` and read it.
fn fetch(up: &Up, remote: &str, results: &Path, name: &str) -> Result<String, String> {
    let local = results.join(name);
    let got = up.ssh.get(&remote.replace('\\', "/"), &local);
    if !got.ok() {
        return Err(format!("{remote} did not copy back: {}", got.tail(2)));
    }
    crate::util::read_regular_text(&local).map_err(|e| format!("{}: {e}", local.display()))
}

/// A point on the primary monitor off the window `w`, in the half beside it with more room.
fn off_window(d: &Display, w: &desk::Rect) -> (i32, i32) {
    let p = d.primary.rect;
    let y = p.t + p.height() / 3;
    if p.r - w.r >= w.l - p.l {
        (w.r + (p.r - w.r) / 2, y)
    } else {
        (p.l + (w.l - p.l) / 2, y)
    }
}

/// One instance of `arch` watched through the phases and walked.
fn record(up: &Up, arch: Arch, display: &Display, results: &Path) -> Result<Recording, String> {
    let s = Session::launch_with(up, arch, INSTANCE, &cpu_config(), false)?;
    let verdict = record_in(up, arch, display, results, &s);
    s.end();
    verdict
}

fn record_in(
    up: &Up,
    arch: Arch,
    display: &Display,
    results: &Path,
    s: &Session<'_>,
) -> Result<Recording, String> {
    let l = &up.layout;
    let pid = s.pid.to_string();
    let place = run_desk(up, &["place", &pid, ATERM_CLASS])?;
    let place = desk::parse_place(&place).ok_or_else(|| format!("place answered {place:?}"))?;
    let half = |n: usize| i32::try_from(n / 2).unwrap_or(0);
    let inside = (
        place.at.0 + half(place.client.0),
        place.at.1 + half(place.client.1),
    );
    let outside = off_window(display, &s.window()?.rect);
    click(l, display, inside)?;
    let log = format!("{}\\uia-events.txt", s.dir);
    let phase = format!("{}\\uia-phase", s.dir);
    let stop = format!("{}\\uia-stop", s.dir);
    set_phase(up, &phase, "start")?;
    let task = task_name(&format!("uia-watch-{}", arch.name()));
    let line = helper_line(&["uia-watch", &pid, ATERM_CLASS, &log, &phase, &stop]);
    let started = run_on_desktop(up, &task, &line, Desktop::Started, false);
    if !started.ok() {
        return Err(format!(
            "the UIA watcher did not start: {}",
            started.tail(5)
        ));
    }
    let log_at: &str = &log;
    let logged = |what: &'static str| {
        move || {
            let out = up.ssh.run(&format!("type {log_at}"));
            if out.text.lines().any(|l| l.trim() == what) {
                Ok(())
            } else {
                Err(out.tail(3))
            }
        }
    };
    let driven = within("the UIA watcher's subscription", logged("ready")).and_then(|()| {
        std::thread::sleep(SETTLE);
        set_phase(up, &phase, "type")?;
        std::thread::sleep(Duration::from_millis(500));
        type_text(l, TYPED)?;
        s.await_text(ECHO)?;
        std::thread::sleep(SETTLE);
        set_phase(up, &phase, "blur")?;
        std::thread::sleep(Duration::from_millis(500));
        click(l, display, outside)?;
        std::thread::sleep(SETTLE);
        set_phase(up, &phase, "focus")?;
        std::thread::sleep(Duration::from_millis(500));
        click(l, display, inside)?;
        std::thread::sleep(SETTLE);
        set_phase(up, &phase, "tab")?;
        std::thread::sleep(Duration::from_millis(500));
        s.ctl(&["tab", "new"])?;
        s.await_text(PROMPT)?;
        std::thread::sleep(SETTLE);
        let acted = run_desk(up, &["uia-act", &pid, ATERM_CLASS, "TabItem", "0"])?;
        if !acted.contains("uia-act ") {
            return Err(format!(
                "the first tab item was not selected: {}",
                crate::util::one_line(acted.trim(), 300)
            ));
        }
        std::thread::sleep(SETTLE);
        set_phase(up, &phase, "find")?;
        std::thread::sleep(Duration::from_millis(500));
        s.ctl(&["find", "open"])?;
        std::thread::sleep(SETTLE);
        s.ctl(&["find", "type", ECHO])?;
        std::thread::sleep(SETTLE);
        set_phase(up, &phase, "end")
    });
    let _ = up.ssh.run(&format!("echo stop> {stop}"));
    let stopped = within("the UIA watcher's end", logged("stopped"));
    let _ = up.admin.ps(&guest::desktop_stop(&task));
    driven?;
    stopped?;
    let events = fetch(
        up,
        &log,
        results,
        &format!("{}-uia-events.txt", arch.name()),
    )?;
    let out = format!("{}\\uia-tree.txt", s.dir);
    let walked = run_desk(up, &["uia-tree", &pid, ATERM_CLASS, ECHO, &out])?;
    if !walked.contains("uia-tree elements=") || walked.contains("marker-missing") {
        return Err(format!(
            "the walk did not find {ECHO}: {}",
            crate::util::one_line(walked.trim(), 300)
        ));
    }
    let tree = fetch(up, &out, results, &format!("{}-uia-tree.txt", arch.name()))?;
    let mut r = compose(arch, &events, &tree);
    r.measured("walk", walked.trim());
    Ok(r)
}

/// One arch's rows: the build, the record, and its comparison with (or, `record`, its write to)
/// the committed record.
fn arch_rows(
    up: &Up,
    arch: Arch,
    repo: &Path,
    display: &Display,
    results: &Path,
    record_it: bool,
) -> Vec<(String, Result<String, String>)> {
    let name = |what: &str| format!("{} uia {what}", arch.name());
    let mut rows = Vec::new();
    let mut env = up.cargo_env(arch);
    env.extend(windows::build_facts(repo));
    let build = ["cargo", "build", "-p", "aterm-gui", "-p", "aterm-ctl"].map(String::from);
    println!("windows: {} ({}) build …", arch.name(), arch.runs());
    let out = up.ssh.streamed(
        &guest::exec_command(guest::SRC, &env, &build),
        &format!("windows: {}: ", arch.name()),
    );
    if !out.ok() {
        rows.push((
            name("build"),
            Err(format!("cargo build failed: {}", out.tail(10))),
        ));
        return rows;
    }
    rows.push((
        name("build"),
        Ok(format!("aterm-gui, aterm-ctl built ({:.0} s)", out.secs)),
    ));
    let fresh = match record(up, arch, display, results) {
        Ok(r) => r,
        Err(e) => {
            rows.push((name("record"), Err(e)));
            return rows;
        }
    };
    let text = fresh.render();
    let _ = crate::util::write_fresh(&results.join(format!("{}-uia.txt", arch.name())), &text);
    rows.push((name("record"), Ok(summary(&fresh))));
    let committed = repo.join(DIR).join(format!("{}.txt", arch.name()));
    if record_it {
        let wrote = std::fs::create_dir_all(repo.join(DIR))
            .and_then(|()| crate::util::write_out(&committed, text.as_bytes()))
            .map(|()| format!("wrote {}", committed.display()))
            .map_err(|e| format!("{}: {e}", committed.display()));
        rows.push((name("recorded"), wrote));
        return rows;
    }
    let held = crate::util::read_regular_text(&committed)
        .map_err(|e| {
            format!(
                "no committed record {} to hold it to ({e}; record one with --record)",
                committed.display()
            )
        })
        .and_then(|t| Recording::parse(&t))
        .and_then(|old| {
            let d = compare(&old, &fresh);
            let moved = render_diff(&d, 20);
            if d.same_behaviour() {
                let n = old.lines.iter().filter(|l| l.class.mark() == '=').count();
                Ok(format!(
                    "identical to {} ({n} compared lines){}",
                    committed.display(),
                    if moved.is_empty() {
                        String::new()
                    } else {
                        format!("; measured moved:\n{moved}")
                    }
                ))
            } else {
                Err(format!("differs from {}:\n{moved}", committed.display()))
            }
        });
    rows.push((name("held"), held));
    rows
}

/// What `uia` was asked.
#[derive(Debug, PartialEq, Eq)]
struct Ask {
    arches: Vec<Arch>,
    /// `--record`: write the committed records instead of holding to them.
    record: bool,
}

fn parse_args(args: &[String]) -> Result<Ask, Fail> {
    let mut ask = Ask {
        arches: Vec::new(),
        record: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--arch" => {
                let w = it.next().map_or("", String::as_str);
                let arch = Arch::parse(w).ok_or_else(|| {
                    Fail::usage(format!("windows uia: --arch {w}: aarch64 or x86_64"))
                })?;
                if !ask.arches.contains(&arch) {
                    ask.arches.push(arch);
                }
            }
            "--record" => ask.record = true,
            other => {
                return Err(Fail::usage(format!(
                    "windows uia: unknown argument '{other}' ({LAB} windows help)"
                )));
            }
        }
    }
    if ask.arches.is_empty() {
        ask.arches = vec![Arch::Aarch64, Arch::X86_64];
    }
    Ok(ask)
}

/// `windows uia [--arch A]... [--record]`.
/// # Errors
/// A usage error, or the lane could not be brought up (not run).
pub fn cmd_uia(args: &[String]) -> Res {
    let ask = parse_args(args)?;
    let l = windows::Layout::find()?;
    let mut lane = windows::boot_lane(&l)?;
    let results = paths().need_state()?.join("windows");
    std::fs::create_dir_all(&results)
        .map_err(|e| Fail::die(format!("{}: {e}", results.display())))?;
    let up = windows::bring_up(&mut lane, false)?;
    let repo = crate::paths::physical(&paths().repo);
    if let Err(e) = windows::copy_tree(&up.layout, &up.ssh, &repo) {
        up.release().map_err(Fail::die)?;
        return Err(not_run(format!("the checkout did not copy: {e}")));
    }
    if let Err(e) = put_helper(&up) {
        up.release().map_err(Fail::die)?;
        return Err(not_run(format!("the desk helper did not copy: {e}")));
    }
    let t0 = Instant::now();
    let mut rows = Vec::new();
    match set_display(&up) {
        Err(e) => rows.push(("display".to_string(), Err(e))),
        Ok(display) => {
            rows.push(("display".to_string(), Ok(display_detail(&display))));
            for &arch in &ask.arches {
                rows.extend(arch_rows(&up, arch, &repo, &display, &results, ask.record));
            }
        }
    }
    let cleanup = up.release();
    let tree = windows::build_facts(&repo)
        .into_iter()
        .find(|(k, _)| k == "ATERM_BUILD_GIT_COMMIT")
        .map_or_else(|| "unknown".to_string(), |(_, v)| v);
    let mut report = format!("tree {tree}\n");
    let mut failed = 0;
    for (row, verdict) in &rows {
        match verdict {
            Ok(d) => report.push_str(&format!("PASS {row}: {d}\n")),
            Err(e) => {
                failed += 1;
                report.push_str(&format!("FAIL {row}: {e}\n"));
            }
        }
    }
    print!("{report}");
    let path = results.join("uia.txt");
    let _ = crate::util::write_out(&path, report.as_bytes());
    println!(
        "windows: {} rows, {failed} failed, {:.0} s, tree {tree}; report {}",
        rows.len(),
        t0.elapsed().as_secs_f64(),
        path.display()
    );
    windows::released(i32::from(failed > 0), cleanup)
}

#[cfg(test)]
#[path = "windows_uia_tests.rs"]
mod tests;
