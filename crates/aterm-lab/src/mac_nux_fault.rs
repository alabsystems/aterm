// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab mac nux`'s fault rows ([`super::FAULTS`]): a first launch or an `aterm init` with one
//! thing going wrong, each in its own guest. A row measures what the build under test did; a part
//! whose product code is not on the build says NOT MEASURED and names the step that brings it
//! (design §10), never a pass.

use std::time::Instant;

use crate::mac::{POLL, ROW, STEP};
use crate::mac_nux_guest::{
    Look, Record, STALL_MS, Stall, Waited, Watch, dialogs, read_census, read_record, read_watch,
    stage_name, stage_starts,
};
use crate::{Fail, stop};

use super::{
    Around, GUEST_NUX, Guest, LOCAL_VAULT, LOCAL_VAULT_ENTRY, LOCAL_VAULT_PORT, OPENED_WATCH,
    PF_GUARD, Row, SETUP_POLL, Value, WATCH, checked_not_ok, drive_cells, lines_cell,
    local_vault_setup, millis, or_missing, quit_app, setup_reading, squeeze, ssh_to_itself, vault,
    wait_setup,
};

/// Why `kill-publish` is NOT RUN: a clone's publish needs a repository to clone.
pub const KILL_PUBLISH: &str = "a kill during a clone's publish needs a repository the guest may clone and a throwaway \
     GitHub token for a test organization (design §14.3, the owner's), which no guest has";

/// Every stage bit the progress record names (`initial.rs` `Stage`).
pub const STAGES: u64 = 0b11_1111;

/// The guest command that lists a running install of the Command Line Tools: Apple's installer,
/// or `softwareupdate` installing them.
const INSTALLERS: &str = "pgrep -lf 'Install Command Line Developer Tools|softwareupdate.*(Command Line|CLTools)' || true";

/// How long `offline` keeps the guest's network down.
const OFFLINE_S: u64 = 30;

/// The guest commands that run `aterm init` under a coding agent: a perl whose `argv[0]` is
/// `claude` (what SAFE-1's ancestry walk reads) runs the command it is handed as its child, and
/// exits with its status.
const AS_AGENT: &str = r#"exec { "/usr/bin/perl" } "claude", "-e",
    'my $s = system { $ARGV[0] } @ARGV; exit($s == -1 ? 127 : ($s & 127) ? 128 + ($s & 127) : $s >> 8)',
    @ARGV;
"#;

/// The stage bits of `bits`, by name (`none` for 0).
#[must_use]
pub fn bits_text(bits: u64) -> String {
    let names: Vec<&str> = (0..6)
        .map(|n| 1u64 << n)
        .filter(|bit| bits & bit != 0)
        .map(stage_name)
        .collect();
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join("+")
    }
}

/// The record as a watch last saw it.
#[must_use]
pub fn last_record(looks: &[Look]) -> Option<Record> {
    looks
        .iter()
        .rev()
        .find_map(|l| l.text.as_deref().and_then(read_record))
}

/// What a launch after a kill did, from its watch: the stage it ran first and when (ms after
/// `from_ms`, the launch, epoch ms), and each stage it ran that the record had done before.
#[derive(Debug, PartialEq, Eq)]
pub struct Resume {
    pub first: Option<(u64, u64)>,
    pub reran: u64,
    /// Every stage it ran.
    pub ran: u64,
}

/// [`Resume`] of a launch's watch, given the stages done before it (`done_before`).
#[must_use]
pub fn resume(looks: &[Look], done_before: u64, from_ms: u64) -> Resume {
    let starts = stage_starts(looks);
    let ran = starts.iter().fold(0, |all, (_, bit)| all | bit);
    Resume {
        first: starts
            .first()
            .map(|(at, bit)| (at.saturating_sub(from_ms), *bit)),
        reran: ran & done_before,
        ran,
    }
}

/// One kill and the launch after it, as a phrase.
#[must_use]
pub fn resume_text(killed: u64, r: &Resume) -> String {
    let first = r.first.map_or_else(
        || "ran no stage".to_owned(),
        |(ms, bit)| format!("ran {} first, {ms} ms after its launch", stage_name(bit)),
    );
    let reran = if r.reran == 0 {
        "re-ran no done stage".to_owned()
    } else {
        format!("RE-RAN done {}", bits_text(r.reran))
    };
    format!("after {}'s kill: {first}; {reran}", stage_name(killed))
}

/// The packets and drops `dnctl pipe show` counts over its pipe's queues; `None` when it shows
/// no queue (no packet went through it).
#[must_use]
pub fn pipe_counts(show: &str) -> Option<(u64, u64)> {
    let mut lines = show
        .lines()
        .skip_while(|l| !l.trim_start().starts_with("BKT"));
    lines.next()?;
    let mut seen = None;
    for line in lines {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(packets), Some(drops)) = (
            f.get(4).and_then(|n| n.parse::<u64>().ok()),
            f.get(8).and_then(|n| n.parse::<u64>().ok()),
        ) else {
            continue;
        };
        let (p, d) = seen.unwrap_or((0, 0));
        seen = Some((p + packets, d + drops));
    }
    seen
}

/// The interface `route -n get default` names.
#[must_use]
pub fn default_interface(route: &str) -> Option<&str> {
    route
        .lines()
        .find_map(|l| l.trim().strip_prefix("interface:"))
        .map(str::trim)
        .filter(|i| !i.is_empty())
}

/// The bundle each running copy of the app runs from, from `ps -axo pid=,command=`: each
/// process whose command is an `aterm.app`'s executable.
#[must_use]
pub fn app_runs(ps: &str) -> Vec<String> {
    const EXE: &str = "/Contents/MacOS/aterm";
    ps.lines()
        .filter_map(|line| {
            let command = line.trim().split_once(' ')?.1.trim();
            let at = command.find(EXE)?;
            let rest = command.get(at + EXE.len()..)?;
            (rest.is_empty() || rest.starts_with(' ')).then(|| command[..at].to_owned())
        })
        .collect()
}

/// Where a bundle runs from, as design §6.8 names a posture.
#[must_use]
pub fn posture(bundle: &str) -> &'static str {
    if bundle.contains("/AppTranslocation/") {
        "translocated"
    } else if bundle.starts_with("/Volumes/") {
        "mounted image"
    } else if bundle.ends_with("/Applications/aterm.app") {
        "installed"
    } else {
        "elsewhere"
    }
}

/// The absence `aterm init` named (`needs a person at this Mac's console (<why>)`), `None` when it
/// named none (it read a person at the console).
#[must_use]
pub fn presence_read(out: &str) -> Option<String> {
    const NEEDS: &str = "needs a person at this Mac's console (";
    let at = out.find(NEEDS)?;
    let rest = out.get(at + NEEDS.len()..)?;
    Some(rest.split_once("): ")?.0.to_owned())
}

/// [`presence_read`] as a cell.
pub(super) fn presence_cell(out: &str) -> Value {
    Value::Text(presence_read(out).map_or_else(
        || "at the console (init named no absence)".to_owned(),
        |why| format!("away: {why}"),
    ))
}

/// A boolean `defaults read` printed; `None` for a key that is not set.
#[must_use]
pub fn defaults_bool(out: &str) -> Option<bool> {
    match out.trim() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// The app's GUI process (its command line is its executable alone), looked for within
/// [`STEP`].
fn app_pid(g: &Guest) -> Result<Option<u32>, Fail> {
    let started = Instant::now();
    loop {
        let out = g.run(&format!("pgrep -f '^{}$' | head -1", g.aterm), STEP)?;
        if let Ok(pid) = out.trim().parse() {
            return Ok(Some(pid));
        }
        if started.elapsed() >= STEP {
            return Ok(None);
        }
        stop::sleep(POLL)?;
    }
}

/// Open the app: the guest's clock at the open (epoch ms), which the row's dialogs count from
/// when no earlier open set the mark.
fn open_app(g: &Guest) -> Result<u64, Fail> {
    let at = g.clock_ms()?;
    g.sh(&format!("open {}", g.app), STEP)?;
    if g.mark_ms.get().is_none() {
        g.mark_ms.set(Some(at));
    }
    Ok(at)
}

/// A watch file in [`GUEST_NUX`], read back.
fn looks(g: &Guest, file: &str) -> Result<Watch, Fail> {
    Ok(read_watch(&g.read(&format!("{GUEST_NUX}/{file}"))?))
}

/// How [`wait_for`] ended.
enum Until {
    /// What the row waited for happened, at this guest epoch ms.
    Hit(u64),
    Done,
    /// Setup said the same for a minute (the one-minute hang detector): what it said.
    Stalled(String),
    /// [`WATCH`] passed with setup still moving.
    Ceiling,
}

/// Wait until `hit` answers (the guest epoch ms of what the row waits for), setup is done, setup
/// stood still a minute with nothing new from the person or in the app's processes, or [`WATCH`]
/// passed.
fn wait_for(g: &Guest, mut hit: impl FnMut() -> Result<Option<u64>, Fail>) -> Result<Until, Fail> {
    let mut stall = Stall::new(STALL_MS);
    let started = Instant::now();
    loop {
        stop::check()?;
        if let Some(at) = hit()? {
            return Ok(Until::Hit(at));
        }
        match stall.see(millis(started.elapsed()), &setup_reading(g)?) {
            Waited::Done => return Ok(Until::Done),
            Waited::Stalled { stood, .. } => return Ok(Until::Stalled(stood)),
            Waited::Moving => {}
        }
        if started.elapsed() >= WATCH {
            return Ok(Until::Ceiling);
        }
        stop::sleep(SETUP_POLL)?;
    }
}

/// What a wait that did not hit came to, as a phrase.
fn until_text(until: &Until) -> String {
    match until {
        Until::Hit(_) => "it happened".to_owned(),
        Until::Done => "setup finished first".to_owned(),
        Until::Stalled(stood) => format!("setup stood at {stood} for a minute"),
        Until::Ceiling => format!("setup still moved at {} min", WATCH.as_secs() / 60),
    }
}

/// Wait for the first stage to start in `file`'s watch: when (guest epoch ms), or why not.
fn first_stage(g: &Guest, file: &str) -> Result<Result<u64, String>, Fail> {
    match wait_for(g, || {
        Ok(stage_starts(&looks(g, file)?.0).first().map(|(at, _)| *at))
    })? {
        Until::Hit(at) => Ok(Ok(at)),
        other => Ok(Err(until_text(&other))),
    }
}

/// The rows a run left not in place, from `init --check --json` (no terminal), under `label`.
fn not_ok_now(g: &Guest, row: &mut Row, label: &str) -> Result<(), Fail> {
    let mut scratch = Row::new(row.scenario);
    checked_not_ok(g, &mut scratch)?;
    for (name, value) in scratch.cells {
        row.cell(format!("{label}{name}"), value);
    }
    Ok(())
}

/// Wait until the app's GUI process `pid` is gone (within [`STEP`]).
fn gone(g: &Guest, pid: u32) -> Result<(), Fail> {
    g.run(
        &format!(
            "n=0; while kill -0 {pid} 2>/dev/null && [ $n -lt {} ]; do n=$((n+1)); sleep 0.1; done",
            STEP.as_secs() * 10
        ),
        STEP + STEP,
    )?;
    Ok(())
}

/// `kill-stage`: the app SIGKILLed at each stage it runs (the first stage it runs twice, the
/// second crash being the one design §6.5 parks), by a guest-side watch of the progress record
/// that kills within 50 ms of the record naming the stage, and opened again after each kill: which
/// stage each launch ran first and whether it re-ran a done stage, then setup to its end.
pub(super) fn kill_stage(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let around = Around::take(g)?;
    let mut mask = STAGES;
    let mut first_killed: Option<u64> = None;
    let mut kills = Vec::new();
    let mut after = Vec::new();
    // The kill before this launch: the stage, and the stages done when it came.
    let mut before: Option<(u64, u64)> = None;
    let mut third_after: Option<u64> = None;
    let mut third = None;
    let mut ended = None;
    for n in 0..=STAGES.count_ones() + 1 {
        let launched = open_app(g)?;
        let Some(pid) = app_pid(g)? else {
            row.fail(format!(
                "the app did not run within {} s of its open",
                STEP.as_secs()
            ));
            break;
        };
        // A stage already running when the watch starts is killed at its first look.
        let file = format!("kill-{n}.txt");
        g.watch_record(&file, (mask != 0).then_some((pid, mask)))?;
        let until = if mask == 0 {
            wait_for(g, || Ok(None))?
        } else {
            wait_for(g, || Ok(looks(g, &file)?.1.first().map(|k| k.0)))?
        };
        let (seen, sent) = looks(g, &file)?;
        if let Some((killed, done)) = before.take() {
            let r = resume(&seen, done, launched);
            if third_after.take() == Some(killed) {
                third = Some(if r.ran & killed != 0 {
                    format!(
                        "{} ran a third time (no park; RUN-1 parks a stage after its second crash)",
                        stage_name(killed)
                    )
                } else {
                    format!("{} did not run again (parked)", stage_name(killed))
                });
            }
            after.push(resume_text(killed, &r));
        }
        let (Until::Hit(at), Some(&(_, _, bit))) = (&until, sent.first()) else {
            ended = Some(until_text(&until));
            break;
        };
        kills.push(format!(
            "{} {} ms",
            stage_name(bit),
            at.saturating_sub(launched)
        ));
        // The first stage killed stays in the mask for a second kill; every other goes at once.
        match first_killed {
            None => first_killed = Some(bit),
            Some(first) => {
                mask &= !bit;
                if first == bit {
                    third_after = Some(bit);
                }
            }
        }
        before = Some((bit, last_record(&seen).map_or(0, |r| r.done)));
        gone(g, pid)?;
    }
    row.cell(
        "kills (ms after each launch)",
        if kills.is_empty() {
            Value::Missing(format!(
                "no stage ran to kill: {}",
                ended.as_deref().unwrap_or("the app did not run")
            ))
        } else {
            Value::Text(kills.join(", "))
        },
    );
    row.cell(
        "after each kill",
        if after.is_empty() {
            Value::Missing("no launch followed a kill".into())
        } else {
            Value::Text(after.join("; "))
        },
    );
    row.cell(
        "after the second crash of one stage",
        or_missing(third, Value::Text, "no stage was killed twice"),
    );
    row.cell(
        "how the last launch ended",
        Value::Text(ended.unwrap_or_else(|| "every stage was killed".to_owned())),
    );
    row.cell(
        "a hung stage",
        Value::Missing(
            "the lane cannot hang one stage without hanging the app; RUN-1's per-stage hang detector brings the park it would measure"
                .into(),
        ),
    );
    let reading = setup_reading(g)?;
    row.cell("setup at the end", Value::Text(reading.said()));
    if !reading.done() {
        not_ok_now(g, row, "")?;
    }
    around.cells(g, row, "")?;
    Ok(())
}

/// `kill-clt`: on a Mac without the Command Line Tools, the app SIGKILLed while an install of
/// them runs (Apple's installer, or `softwareupdate` installing them), then opened again: whether
/// the install outlived the kill and what the relaunch did with it.
pub(super) fn kill_clt(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    if g.run("xcode-select -p >/dev/null 2>&1 && echo present", STEP)?
        .trim()
        == "present"
    {
        row.cell(
            "kill during the Command Line Tools",
            Value::NotRun(
                "this guest has the Command Line Tools: run it with --no-clt or --vanilla".into(),
            ),
        );
        return Ok(());
    }
    g.watch_record("watch.txt", None)?;
    let launched = open_app(g)?;
    let Some(pid) = app_pid(g)? else {
        row.fail(format!(
            "the app did not run within {} s of its open",
            STEP.as_secs()
        ));
        return Ok(());
    };
    let until = wait_for(g, || {
        let seen = g.run(INSTALLERS, STEP)?;
        Ok(if seen.trim().is_empty() {
            None
        } else {
            Some(g.clock_ms()?)
        })
    })?;
    let Until::Hit(at) = until else {
        row.cell(
            "kill during the Command Line Tools",
            Value::Missing(format!(
                "no install of the Command Line Tools ran in this launch ({}); RUN-4 starts it at the walk's authorization",
                until_text(&until)
            )),
        );
        return Ok(());
    };
    row.cell("install seen", Value::Ms(at.saturating_sub(launched)));
    g.run(&format!("kill -KILL {pid}"), STEP)?;
    gone(g, pid)?;
    stop::sleep(std::time::Duration::from_secs(5))?;
    let outlived = g.run(INSTALLERS, STEP)?;
    row.cell(
        "install 5 s after the kill",
        Value::Text(if outlived.trim().is_empty() {
            "gone".to_owned()
        } else {
            format!("still running ({})", squeeze(outlived.trim(), 100))
        }),
    );
    let relaunched = open_app(g)?;
    wait_setup(
        g,
        row,
        "relaunched: ",
        g.clock_ms()?.saturating_sub(relaunched),
    )?;
    let installers = g.run(INSTALLERS, STEP)?;
    row.cell(
        "installers after the relaunch",
        Value::Count(installers.lines().filter(|l| !l.trim().is_empty()).count() as u64),
    );
    let developer =
        g.vm.exec("xcode-select -p 2>&1; echo \"exit=$?\"", STEP)
            .map_err(Fail::die)?;
    row.cell(
        "xcode-select -p at the end",
        Value::Text(super::xcode_select_text(&String::from_utf8_lossy(
            &developer.stdout,
        ))),
    );
    Ok(())
}

/// The stage starts `file`'s watch saw, in ms after `launched` (epoch ms), as a cell under
/// `label`.
fn starts_cell(
    g: &Guest,
    row: &mut Row,
    label: &str,
    file: &str,
    launched: u64,
) -> Result<(), Fail> {
    let starts = stage_starts(&looks(g, file)?.0);
    row.cell(
        format!("{label}stage starts"),
        Value::Text(if starts.is_empty() {
            "none".to_owned()
        } else {
            starts
                .iter()
                .map(|(at, bit)| format!("{} {} ms", stage_name(*bit), at.saturating_sub(launched)))
                .collect::<Vec<_>>()
                .join(", ")
        }),
    );
    Ok(())
}

/// One launch run to setup's end (or its hang detector), a fresh watch `file` started before the
/// open: its end and stage starts under `label`, and its open (epoch ms).
fn launch_to_end(g: &Guest, row: &mut Row, label: &str, file: &str) -> Result<u64, Fail> {
    g.watch_record(file, None)?;
    let launched = open_app(g)?;
    wait_setup(g, row, label, g.clock_ms()?.saturating_sub(launched))?;
    starts_cell(g, row, label, file, launched)?;
    Ok(launched)
}

/// The dialogs the census has seen since `since_ms`, as a cell (read while it still runs).
fn dialogs_since(g: &Guest, since_ms: u64) -> Result<Value, Fail> {
    let census = read_census(&g.read(&format!("{GUEST_NUX}/census.txt"))?);
    Ok(match census.error {
        Some(why) => Value::Missing(format!("the census could not read the window list: {why}")),
        None => Value::Text(super::dialog_text(&dialogs(&census, since_ms))),
    })
}

/// `rerun`: a first launch to setup's end, then the app quit and opened again (its stages,
/// changes and dialogs: 0 each wanted), then a second `aterm init` on a terminal (0 changes, 0
/// prompts, at most 3 lines wanted).
pub(super) fn rerun(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    launch_to_end(g, row, "first launch: ", "first.txt")?;
    quit_app(g)?;
    let around = Around::take(g)?;
    let second = launch_to_end(g, row, "second launch: ", "second.txt")?;
    row.cell("second launch: dialogs", dialogs_since(g, second)?);
    around.cells(g, row, "second launch: ")?;
    let around = Around::take(g)?;
    let (d, out) = g.drive(
        "rerun-init",
        &format!("NUX_SUDO={}", g.sudo_password()),
        &format!("{} init", g.aterm),
    )?;
    drive_cells(row, "second init: ", &d);
    lines_cell(row, "second init: ", &out);
    around.cells(g, row, "second init: ")?;
    if d.exit != Some(0) {
        not_ok_now(g, row, "second init: ")?;
    }
    // The census counts from the second launch on.
    g.mark_ms.set(Some(second));
    Ok(())
}

/// The Dock keys the person's change sets, each against the profile's value.
const PERSON_DOCK: &str = "defaults write com.apple.dock show-recents -bool true; \
                           defaults write com.apple.dock tilesize -int 36; killall Dock";

/// The person's Dock keys as they read now.
fn person_dock(g: &Guest) -> Result<String, Fail> {
    let recents = g.run(
        "defaults read com.apple.dock show-recents 2>/dev/null",
        STEP,
    )?;
    let size = g.run("defaults read com.apple.dock tilesize 2>/dev/null", STEP)?;
    Ok(format!(
        "show-recents {}, tilesize {}",
        defaults_bool(&recents).map_or("unset", |b| if b { "on" } else { "off" }),
        if size.trim().is_empty() {
            "unset"
        } else {
            size.trim()
        }
    ))
}

/// `legacy-upgrade`: a Mac an older aterm (0.109) left with a pending `first-run.json` and a Dock
/// the person changed: whether this build's launch starts setup, and whether it keeps the person's
/// Dock.
pub(super) fn legacy_upgrade(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    g.sh(
        &format!(
            "mkdir -p \"$HOME/Library/Application Support/aterm/doctor\" && \
             printf '%s' '{{\"profile\":\"macos-workstation-v1\",\"state\":\"pending\"}}' > \"$HOME/Library/Application Support/aterm/doctor/first-run.json\"; \
             {PERSON_DOCK}"
        ),
        STEP,
    )?;
    row.cell("the person's Dock before", Value::Text(person_dock(g)?));
    let around = Around::take(g)?;
    launch_to_end(g, row, "", "watch.txt")?;
    row.cell("the person's Dock after", Value::Text(person_dock(g)?));
    row.cell(
        "\"Changed by you\"",
        Value::Missing("RUN-1 brings the side record that keeps a person's key".into()),
    );
    around.cells(g, row, "")?;
    Ok(())
}

/// `person-change`: setup run to its end, then the person turns the Dock's recent apps on (the
/// profile turns them off) and the app is opened again: whether the relaunch reverts it.
pub(super) fn person_change(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    launch_to_end(g, row, "first launch: ", "first.txt")?;
    g.sh(PERSON_DOCK, STEP)?;
    row.cell("the person's Dock", Value::Text(person_dock(g)?));
    quit_app(g)?;
    let around = Around::take(g)?;
    launch_to_end(g, row, "relaunch: ", "second.txt")?;
    row.cell(
        "the person's Dock after the relaunch",
        Value::Text(person_dock(g)?),
    );
    around.cells(g, row, "relaunch: ")?;
    row.cell(
        "after a widen",
        Value::NotRun(
            "a widen needs a late ALab signal (the ALab vault, or SYNC-1's lab service type), which no guest has".into(),
        ),
    );
    row.cell(
        "after a wake",
        Value::Missing(
            "a guest booted with no graphics cannot be slept and woken by the lane".into(),
        ),
    );
    row.cell(
        "a closed-unanswered review's successor",
        Value::Missing(
            "the rev-3 closed-review record has no format the lane can seed (SCR-1 deletes it); legacy-upgrade seeds the Pending record"
                .into(),
        ),
    );
    Ok(())
}

/// The progress record's first change at or after `since_ms` (guest epoch ms) that differs from
/// the look before it: ms after `since_ms`.
#[must_use]
pub fn first_change_after(looks: &[Look], since_ms: u64) -> Option<u64> {
    let before = looks
        .iter()
        .rev()
        .find(|l| l.at_ms < since_ms)
        .map(|l| &l.text);
    looks
        .iter()
        .filter(|l| l.at_ms >= since_ms)
        .find(|l| Some(&l.text) != before)
        .map(|l| l.at_ms - since_ms)
}

/// `offline`: the guest's network down from just before the open for [`OFFLINE_S`], brought back
/// by the same root step (so it returns even if `tart exec` rode it): whether `tart exec` answers
/// while it is down, setup's first progress after it is back, and the rows left not in place.
pub(super) fn offline(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let route = g.run("route -n get default 2>/dev/null", STEP)?;
    let Some(iface) = default_interface(&route).map(str::to_owned) else {
        row.fail(format!(
            "the guest has no default route: {}",
            squeeze(route.trim(), 120)
        ));
        return Ok(());
    };
    g.watch_record("watch.txt", None)?;
    let down_ms = g.clock_ms()?;
    g.sh(
        &format!(
            "({} </dev/null >/dev/null 2>&1 &); sleep 0.5",
            g.as_root(&format!(
                "ifconfig {iface} down; sleep {OFFLINE_S}; ifconfig {iface} up"
            ))
        ),
        STEP,
    )?;
    let launched = open_app(g)?;
    let answered =
        g.vm.exec("route -n get default >/dev/null 2>&1; echo $?", STEP);
    row.cell(
        format!("tart exec with {iface} down"),
        Value::Text(match answered {
            Ok(out) => format!(
                "answers (it does not ride {iface}; default route lookup exit {})",
                String::from_utf8_lossy(&out.stdout).trim()
            ),
            Err(why) => format!("no answer: {}", squeeze(&why, 100)),
        }),
    );
    let up_ms = down_ms + OFFLINE_S * 1000;
    let back = Instant::now();
    while !g
        .run("route -n get default >/dev/null 2>&1 && echo up", STEP)?
        .contains("up")
    {
        if back.elapsed() >= ROW {
            row.fail(format!(
                "{iface} did not come back within {} s",
                ROW.as_secs()
            ));
            return Ok(());
        }
        stop::sleep(POLL)?;
    }
    wait_setup(g, row, "", g.clock_ms()?.saturating_sub(launched))?;
    let (seen, _) = looks(g, "watch.txt")?;
    row.cell(
        "first progress after the network came back",
        or_missing(
            first_change_after(&seen, up_ms),
            Value::Ms,
            "setup's record did not change after the network came back",
        ),
    );
    starts_cell(g, row, "", "watch.txt", launched)?;
    not_ok_now(g, row, "after: ")?;
    Ok(())
}

/// The packet filter `lossy` loads: every packet but loopback's through dummynet pipe 1, and the
/// guard ([`PF_GUARD`]) as every row has it.
fn lossy_rules() -> String {
    format!(
        "dummynet out quick proto {{ tcp udp icmp }} from any to ! 127.0.0.0/8 pipe 1\n\
         dummynet in quick proto {{ tcp udp icmp }} from ! 127.0.0.0/8 to any pipe 1\n{PF_GUARD}"
    )
}

/// Load `rules` (named `name` in the guest folder) as the guest's packet filter: the rules and
/// dummynet rules it then holds, which must still hold the guard.
fn pf_load(g: &Guest, name: &str, rules: &str) -> Result<String, Fail> {
    g.sh(
        &format!("cat > {GUEST_NUX}/{name} <<'EOF'\n{rules}EOF"),
        STEP,
    )?;
    let held = g.sh(
        &g.as_root(&format!(
            "/sbin/pfctl -q -f {GUEST_NUX}/{name} 2>&1; /sbin/pfctl -q -s rules 2>/dev/null; /sbin/pfctl -q -s dummynet 2>/dev/null"
        )),
        STEP,
    )?;
    if !held.contains("7484") {
        return Err(Fail::die(format!(
            "the guest's packet filter lost the vault port's null route: {}",
            squeeze(held.trim(), 200)
        )));
    }
    Ok(held)
}

/// `lossy`: a first launch through a 1 Mbit/s link with 150 ms delay and 1% loss (dnctl pipe 1,
/// every packet but loopback's): setup's end, the stages, the rows left not in place under it and
/// once the link is clean, and the packets the pipe carried (its proof).
pub(super) fn lossy(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    g.sh(
        &g.as_root("/usr/sbin/dnctl pipe 1 config bw 1Mbit/s delay 150 plr 0.01"),
        STEP,
    )?;
    let held = pf_load(g, "pf-lossy.conf", &lossy_rules())?;
    if !held.contains("pipe 1") {
        return Err(Fail::die(format!(
            "the guest's packet filter did not take the dummynet rules: {}",
            squeeze(held.trim(), 200)
        )));
    }
    launch_to_end(g, row, "", "watch.txt")?;
    not_ok_now(g, row, "shaped: ")?;
    let show = g.run(&g.as_root("/usr/sbin/dnctl pipe show 1"), STEP)?;
    row.cell(
        "pipe 1 carried",
        match pipe_counts(&show) {
            Some((packets, drops)) => Value::Text(format!("{packets} packets, {drops} dropped")),
            None => Value::Missing(format!(
                "dnctl showed no queue: {}",
                squeeze(show.trim(), 120)
            )),
        },
    );
    pf_load(g, "pf.conf", PF_GUARD)?;
    g.sh(&g.as_root("/usr/sbin/dnctl -q flush"), STEP)?;
    not_ok_now(g, row, "clean again: ")?;
    Ok(())
}

/// `mdns-silent` / `mdns-no-route`: the vault row ([`vault`]) with the guest's mDNS dropped
/// (silent: queries go out and nothing answers) or refused (`no_route`: each send refused at
/// once), then the rows left not in place once mDNS is back.
pub(super) fn mdns(g: &Guest, row: &mut Row, no_route: bool) -> Result<(), Fail> {
    let action = if no_route {
        "block return"
    } else {
        "block drop"
    };
    pf_load(
        g,
        "pf-mdns.conf",
        &format!("{action} out quick proto udp from any to any port 5353\n{PF_GUARD}"),
    )?;
    row.cell(
        "mDNS",
        Value::Text(format!("{action} out udp 5353 (the packet filter)")),
    );
    vault(g, row)?;
    pf_load(g, "pf.conf", PF_GUARD)?;
    not_ok_now(g, row, "mDNS back: ")?;
    Ok(())
}

/// One open of the app from a mounted image: where it ran (or that it did not, within
/// [`OPENED_WATCH`]), the Gatekeeper dialogs since the open, and whether an installed copy
/// appeared; every copy quit after.
fn dmg_open(g: &Guest, row: &mut Row, label: &str, volume: &str) -> Result<(), Fail> {
    let at = g.clock_ms()?;
    g.sh(&format!("open '{volume}/aterm.app' 2>&1 || true"), STEP)?;
    let started = Instant::now();
    let runs = loop {
        let runs = app_runs(&g.run("ps -axo pid=,command=", STEP)?);
        if !runs.is_empty() || started.elapsed() >= OPENED_WATCH {
            break runs;
        }
        stop::sleep(POLL)?;
    };
    row.cell(
        format!("{label}runs from"),
        if runs.is_empty() {
            Value::Text(format!("nothing ran within {} s", OPENED_WATCH.as_secs()))
        } else {
            Value::Text(format!(
                "{} after {} ms",
                runs.iter()
                    .map(|b| format!("{} ({b})", posture(b)))
                    .collect::<Vec<_>>()
                    .join(", "),
                millis(started.elapsed())
            ))
        },
    );
    let census = read_census(&g.read(&format!("{GUEST_NUX}/census.txt"))?);
    let gatekeeper = dialogs(&census, at)
        .iter()
        .filter(|d| d.kind == "Gatekeeper")
        .map(|d| d.windows)
        .sum::<usize>();
    row.cell(
        format!("{label}Gatekeeper dialogs"),
        Value::Count(gatekeeper as u64),
    );
    let installed = g.run(&format!("[ -d '{}' ] && echo yes || echo no", g.app), STEP)?;
    row.cell(
        format!("{label}installed copy"),
        Value::Text(installed.trim().to_owned()),
    );
    g.run(
        "pkill -KILL -f '/aterm.app/Contents/MacOS/aterm$' || true; pkill -x CoreServicesUIAgent || true; sleep 1",
        STEP,
    )?;
    Ok(())
}

/// `dmg`: the app in a disk image built in the guest (`hdiutil`), opened twice from the mounted
/// image as a tool copied it, then twice from the same image quarantined as a browser download:
/// where it ran (installed, mounted image, translocated), the Gatekeeper dialogs each open met,
/// and whether a copy was installed (RUN-3 installs and relaunches it).
pub(super) fn dmg(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let started = Instant::now();
    g.sh(
        &format!(
            "set -e; mkdir -p {GUEST_NUX}/dmg-src; mv '{app}' {GUEST_NUX}/dmg-src/; \
             hdiutil create -quiet -volname aterm -srcfolder {GUEST_NUX}/dmg-src -ov -format UDZO {GUEST_NUX}/aterm.dmg; \
             rm -rf {GUEST_NUX}/dmg-src",
            app = g.app
        ),
        ROW,
    )?;
    row.cell(
        "image built in the guest",
        Value::Ms(millis(started.elapsed())),
    );
    for (label, quarantine) in [("mounted", false), ("downloaded", true)] {
        if quarantine {
            g.sh(
                &format!(
                    "xattr -w com.apple.quarantine \"0083;$(printf %x $(date +%s));Safari;\" {GUEST_NUX}/aterm.dmg"
                ),
                STEP,
            )?;
        }
        let attached = g.sh(
            &format!("hdiutil attach -nobrowse -noautoopen {GUEST_NUX}/aterm.dmg"),
            ROW,
        )?;
        let Some(volume) = attached
            .lines()
            .rev()
            .find_map(|l| l.find("/Volumes/").map(|at| l[at..].trim().to_owned()))
        else {
            row.fail(format!(
                "hdiutil attach named no volume: {}",
                squeeze(attached.trim(), 120)
            ));
            return Ok(());
        };
        for open in ["1st", "2nd"] {
            dmg_open(g, row, &format!("{label}, {open} open: "), &volume)?;
        }
        g.run(
            &format!(
                "hdiutil detach -force '{volume}' >/dev/null 2>&1; {}",
                g.as_root(&format!("rm -rf '{}'", g.app))
            ),
            STEP,
        )?;
    }
    Ok(())
}

/// `agent-ssh`: `aterm init` under a coding agent (a parent named `claude`) on a terminal, and over
/// `ssh -tt` to the guest itself, each with the presence it read and what it changed; then the
/// off-console sync: a vault of the guest's own joined over SSH (0 Dock and preference calls
/// wanted; SYNC-1 brings a join with no prompt).
pub(super) fn agent_ssh(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let sudo = format!("NUX_SUDO={}", g.sudo_password());
    g.sh(
        &format!("cat > {GUEST_NUX}/as-agent.pl <<'EOF'\n{AS_AGENT}EOF"),
        STEP,
    )?;
    ssh_to_itself(g)?;
    let ssh = format!(
        "ssh -tt -o BatchMode=yes -o StrictHostKeyChecking=no -i {GUEST_NUX}/key localhost"
    );
    for (tag, label, cmd) in [
        (
            "agent-init",
            "agent: ",
            format!("/usr/bin/perl {GUEST_NUX}/as-agent.pl {} init", g.aterm),
        ),
        ("ssh-init", "ssh: ", format!("{ssh} {} init", g.aterm)),
    ] {
        let around = Around::take(g)?;
        let (d, out) = g.drive(tag, &sudo, &cmd)?;
        row.cell(format!("{label}presence read"), presence_cell(&out));
        drive_cells(row, label, &d);
        lines_cell(row, label, &out);
        around.cells(g, row, label)?;
    }
    let admin = format!("{GUEST_NUX}/vault-admin");
    let signer = g.sh(&local_vault_setup(&g.aterm, &admin), STEP)?;
    let signer = signer.trim();
    if signer.is_empty() || signer.contains(char::is_whitespace) {
        return Err(Fail::die(format!(
            "the local vault's signer did not read back: {}",
            g.read(&format!("{admin}/create.out"))?.trim()
        )));
    }
    let url = format!("http://127.0.0.1:{LOCAL_VAULT_PORT}/v1/{LOCAL_VAULT}#{signer}");
    let label = "ssh sync: ";
    let around = Around::take(g)?;
    let (d, out) = g.drive(
        "ssh-sync",
        &format!("NUX_JOIN=\"$(cat {admin}/password)\" {sudo}"),
        &format!("{ssh} {} init --only workstation --vault '{url}'", g.aterm),
    )?;
    row.cell(format!("{label}presence read"), presence_cell(&out));
    drive_cells(row, label, &d);
    let synced = g.run(
        &format!("[ -f ~/.secrets/{LOCAL_VAULT_ENTRY} ] && echo yes || echo no"),
        STEP,
    )?;
    row.cell(
        format!("{label}secret on disk"),
        Value::Text(synced.trim().to_owned()),
    );
    around.cells(g, row, label)?;
    Ok(())
}

/// `clock-back`: the guest's clock moved back an hour once the first stage runs: setup's end, the
/// stages it ran after (a done stage re-run named), and the rows left not in place.
pub(super) fn clock_back(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    g.watch_record("watch.txt", None)?;
    let launched = open_app(g)?;
    let first = match first_stage(g, "watch.txt")? {
        Ok(at) => at,
        Err(why) => {
            row.cell(
                "clock moved back",
                Value::Missing(format!("no stage ran to move the clock under: {why}")),
            );
            return Ok(());
        }
    };
    let (so_far, _) = looks(g, "watch.txt")?;
    let done_before = last_record(&so_far).map_or(0, |r| r.done);
    let moved = g.run(
        &format!(
            "{} >/dev/null 2>&1; perl -MTime::HiRes=time -e 'printf \"%d\\n\", time * 1000'",
            g.as_root("date $(date -v-1H +%m%d%H%M%Y.%S)")
        ),
        STEP,
    )?;
    let moved_ms: u64 = moved.trim().parse().unwrap_or(0);
    row.cell(
        "clock moved back",
        Value::Text(format!(
            "1 h, {} ms after the launch (the guest then read {})",
            first.saturating_sub(launched),
            if moved_ms + 3_000_000 < first {
                "an hour earlier"
            } else {
                "the same hour: the move did not take"
            }
        )),
    );
    wait_setup(g, row, "", 0)?;
    // The watch's clock moved with the guest's: the looks after the move are those past the
    // ones it had written before.
    let (seen, _) = looks(g, "watch.txt")?;
    let r = resume(
        seen.get(so_far.len()..).unwrap_or(&[]),
        done_before,
        moved_ms,
    );
    row.cell(
        "after the move",
        Value::Text(format!(
            "ran {}; {}",
            bits_text(r.ran),
            if r.reran == 0 {
                "re-ran no done stage".to_owned()
            } else {
                format!("RE-RAN done {}", bits_text(r.reran))
            }
        )),
    );
    not_ok_now(g, row, "")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(at_ms: u64, text: &str) -> Look {
        Look {
            at_ms,
            text: (text != "absent").then(|| text.to_owned()),
        }
    }

    #[test]
    fn a_launch_after_a_kill_names_its_first_stage_and_any_done_stage_it_re_ran() {
        let launch = 10_000;
        let looks = [
            look(10_100, "absent"),
            look(11_000, r#"{"state":"pending","done":1,"running":2}"#),
            look(12_000, r#"{"state":"pending","done":3,"running":4}"#),
            look(13_000, r#"{"state":"completed","done":63}"#),
        ];
        let r = resume(&looks, 1, launch);
        assert_eq!(r.first, Some((1000, 2)));
        assert_eq!((r.reran, r.ran), (0, 6));
        assert_eq!(
            resume_text(1, &r),
            "after Workstation's kill: ran Spotlight first, 1000 ms after its launch; re-ran no done stage"
        );
        // NEGATIVE CONTROL: a launch that ran Workstation again after it was done names it.
        let again = [
            look(10_500, r#"{"state":"pending","done":1,"running":1}"#),
            look(11_500, r#"{"state":"pending","done":1,"running":2}"#),
        ];
        let r = resume(&again, 1, launch);
        assert_eq!(r.reran, 1);
        assert!(
            resume_text(2, &r).ends_with("RE-RAN done Workstation"),
            "{}",
            resume_text(2, &r)
        );
        let none = resume(&[look(10_100, "absent")], 0, launch);
        assert_eq!((none.first, none.ran), (None, 0));
        assert!(resume_text(4, &none).contains("ran no stage"));
        assert_eq!(bits_text(0), "none");
        assert_eq!(bits_text(1 | 8 | 32), "Workstation+Secrets+Accounts");
        assert_eq!(
            last_record(&looks).map(|r| r.state),
            Some("completed".to_owned())
        );
        assert_eq!(last_record(&[look(1, "absent")]), None);
    }

    #[test]
    fn the_first_change_after_the_network_returns_is_timed_from_its_return() {
        let looks = [
            look(1_000, r#"{"state":"pending","done":0,"running":1}"#),
            look(40_000, r#"{"state":"pending","done":0,"running":1}"#),
            look(41_250, r#"{"state":"pending","done":1,"running":2}"#),
        ];
        // The look at 40 000 says what the one before the return said: no progress yet.
        assert_eq!(first_change_after(&looks, 31_000), Some(10_250));
        assert_eq!(first_change_after(&looks, 50_000), None);
        // With no look before the return, the first look after it is the change.
        assert_eq!(first_change_after(&looks, 0), Some(1_000));
    }

    #[test]
    fn the_shaping_pipe_route_processes_presence_and_dock_read_from_their_tools() {
        let show = "00001:   1.000 Mbit/s  150 ms   50 sl.plr 0.010000 2 queues (64 buckets) droptail\n    \
                    mask: 0x00 0x00000000/0x0000 -> 0x00000000/0x0000\n\
                    BKT Prot ___Source IP/port____ ____Dest. IP/port____ Tot_pkt/bytes Pkt/Byte Drp\n  \
                    0 tcp     192.168.64.5/49234   17.253.144.10/443    1093  1412245  0    0   12\n  \
                    1 udp     192.168.64.5/5353    224.0.0.251/5353       7     420  0    0   0\n";
        assert_eq!(pipe_counts(show), Some((1100, 12)));
        // NEGATIVE CONTROL: a pipe that carried nothing shows no queue, never 0 packets.
        assert_eq!(pipe_counts("00001:   1.000 Mbit/s  150 ms\n"), None);

        let route = "   route to: default\ndestination: default\n    gateway: 192.168.64.1\n  interface: en0\n";
        assert_eq!(default_interface(route), Some("en0"));
        assert_eq!(
            default_interface("route: writing to routing socket: not in table\n"),
            None
        );

        let ps = "  412 /Applications/aterm.app/Contents/MacOS/aterm\n  \
                  413 /Applications/aterm.app/Contents/MacOS/aterm-helper --x\n  \
                  500 /Volumes/aterm/aterm.app/Contents/MacOS/aterm --restore\n  \
                  501 /private/var/folders/x/T/AppTranslocation/AB12/d/aterm.app/Contents/MacOS/aterm\n  \
                  9 /usr/bin/grep aterm\n";
        let runs = app_runs(ps);
        assert_eq!(runs.len(), 3, "{runs:?}");
        assert_eq!(
            runs.iter().map(|b| posture(b)).collect::<Vec<_>>(),
            ["installed", "mounted image", "translocated"]
        );
        assert_eq!(posture("/tmp/aterm-nux/aterm.app"), "elsewhere");

        let away = "Workstation  deferred  needs a person at this Mac's console (a coding agent started this): run aterm init there\n";
        assert_eq!(
            presence_read(away).as_deref(),
            Some("a coding agent started this")
        );
        assert_eq!(presence_read("Workstation ok\n"), None);
        assert_eq!(
            presence_cell(away),
            Value::Text("away: a coding agent started this".into())
        );

        assert_eq!(defaults_bool("1\n"), Some(true));
        assert_eq!(defaults_bool("0"), Some(false));
        assert_eq!(defaults_bool(""), None);
    }

    #[test]
    fn the_shaped_and_mdns_filters_keep_the_guard_and_the_agent_runs_as_claude() {
        let rules = lossy_rules();
        assert!(rules.ends_with(PF_GUARD), "{rules}");
        assert!(
            rules
                .lines()
                .take(2)
                .all(|l| l.contains("pipe 1") && l.contains("127.0.0.0/8"))
        );
        // The agent's perl names itself `claude` before it runs the command as its child.
        assert!(AS_AGENT.starts_with(r#"exec { "/usr/bin/perl" } "claude""#));
        assert!(AS_AGENT.contains("system { $ARGV[0] } @ARGV"));
        assert!(KILL_PUBLISH.contains("token"));
        assert_eq!(STAGES.count_ones(), 6);
    }
}
