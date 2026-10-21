// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `restore [tag] quit|crash [--keep] [--land|--land-open]`: the cold restore against the real
//! Claude Code. When aterm quits or crashes while an agent runs, the next
//! launch reopens the tab and the harness relaunches the agent there, on its
//! conversation, in the Claude home and on the login it ran under
//! (`relaunch::after_host_ended`).
//!
//! A lab of its own (`restore-<pid>` unless a tag is given; trust seeded, the
//! headless-restore seam on: a headless instance then keeps its desk as a
//! window does — `ATERM_DEBUG_HEADLESS_RESTORE`, a debug build or one with
//! `--features dev-seams`; aterm-gui's `desk::Desk`). Claude Code takes a turn;
//! the run waits until the crash journal names its conversation, then aterm
//! ends: `quit` sends `invoke Quit`, `crash` SIGKILLs the instance. The
//! instance and its tree end (one still alive after 15 s is said and ended:
//! it would hold the conversation), the socket and its token are removed from
//! outside the jail (the jail denies a jailed process unlinking them), and the
//! instance starts again on the same state. The report: the relaunch's typed
//! line (the upgrade ledger's rows), whether the agent resumed the SAME
//! conversation or sits in onboarding, the relaunch's carry-on, and the
//! timings. The plain run is the Cruise control: its tab under Cruise before
//! aterm ends (a config default of Land or Off cannot run, exit 3), and once
//! the agent is back the restored tab's journal is watched (at most
//! [`LAND_WATCH`]) until its carry-on settles — past the waits and another
//! sweep's lock, as the Land runs read it ([`carry_on_settled`]) — which
//! must be `HOST … carry-on step=continued` with the ledger's `continued`
//! row, no word of Land's (`done:landing`, `done:said-done`) among its
//! steps, and the tab under Cruise ([`cruise_judged`]): Cruise's carry-on
//! typed into open work. Exit 0 resumed with that carry-on; 1 it did not
//! (another conversation, none within 90 s, onboarding, another carry-on or
//! none, an instance that would not end or come back); 2 usage; 3 could not
//! run. NOT OBSERVED: a windowed aterm's own quit (the Apple Event, the
//! confirm), a person's ⌘Q, a crash that leaves the agent running (the step
//! is `crash --keep-tree` under a PTY keeper, [`crate::crash`], judged by its
//! driver), an account move. (A Codex is restored by no lane since the Codex
//! screen lane's retirement, 2026-10-16: codex-alab's restore is
//! `codex-alab-proof`'s.)
//!
//! `--hold` (crash only: a held tab refuses `invoke Quit`, `ERR halted`): the
//! owner holds the tab (`hold <sid> on reason=review`) once the journal names
//! the conversation, and the run waits until the journal carries the hold too
//! (ruling 23 of `docs/HANDOFF-robustness-rounds-2026-10-08.md`): the live
//! instance's own journal, on the tab's leaf (30 s; a journal that never
//! carries it FAILS the run there, aterm not ended). After the
//! restart the report is the held tab's: no agent relaunched and nothing typed
//! (30 s), the reopened tab's `hold=`, the record the restore said of it
//! (`messages tag=harness`, its key `harness.restored-held.<sid>`), the lift
//! command that record names run AS WRITTEN, and the tab's `hold=` after it.
//! Exit 0 when every one of those is what the ruling says; 1 otherwise.
//!
//! `--clear` (LC-1 of ATERM-KI-1015-1): after the turn the run types `/clear`,
//! waits until Claude Code's record names the new conversation and the desk
//! names it too (30 s; said when it does not), then ends aterm: the relaunch
//! must resume the NEW conversation, never the one the agent left, and its
//! carry-on settle on `done:taskless` with nothing typed — nobody has asked
//! the new conversation anything ([`cruise_step`]). It takes no `--land` or
//! `--land-open` (usage, exit 2): taskless, the relaunch types nothing
//! whatever the Autopilot, and Land has no work to land or leave open.
//!
//! `--land` (§9.4.5 and §9.4.8 of `docs/DESIGN-harness-autopilot-2026-10-15.md`):
//! after the turn the tab goes under Land — a tab under Land already (a
//! config default) cannot run, exit 3: its restored Land would prove nothing
//! of the layout — and the run waits until the crash journal carries the
//! word, as `--hold` waits for its hold. The stub's answers become clean
//! defenses, a few lines starting `Nothing left:` (the lab's project is no
//! repository, so its probe finds nothing unlanded), and the run waits (6
//! min) until the tab's `status` says `task=landed` — the short turn's
//! back-off, the land instruction typed, its defense. After the restart the
//! restored tab is watched for [`LAND_WATCH`], past the back-off an attach
//! waits before it types: its task must end `landed`, with no land
//! instruction typed since the relaunch (the ledger's `land@v1` rows), the
//! tab under Land (`status` `autopilot=land`), and the relaunch's carry-on
//! settled with nothing typed (the restored tab's journal's carry-on settles
//! on `HOST … carry-on step=done:said-done`, [`carry_on_judged`], and never
//! says `step=continued`) — no line typed into finished work. Exit 0 only
//! when that holds and the conversation resumed.
//!
//! `--land-open`, `--land`'s negative control: the tab goes under Land
//! mid-work (the stub's ordinary reply, no done; a tab under Land already
//! cannot run, as with `--land`) and aterm ends as soon as the crash journal
//! carries the word, as `--land` waits for it. After the restart the
//! restored tab must be under Land and its journal's carry-on must settle on
//! `HOST … carry-on step=done:landing` within [`LAND_WATCH`] — past the waits
//! and another sweep's lock, which the host settles again after
//! (`busy:another-sweep`), and on no other word: a failed relaunch, a
//! refusal or a word the lab was never taught settles it too, and fails —
//! never `done:said-done`, and never `continued` — with no carry-on line
//! typed: Land's work that was not done is not read as done, and under Land
//! no relaunch types Cruise's carry-on (`upgrade_drive::Opts::land`, F2);
//! the point is the land policy's. Exit 0 only when it does and the
//! conversation resumed.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::LAB;
use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::procs::{alive, descendants};
use crate::pyjson::J;
use crate::run::{Run, is_run};
use crate::stop;
use crate::util::now_ms;
use crate::{Fail, Res};

/// `--land`'s stub mode: every answer a clean defense, as Land's instruction
/// asks for it — so the instruction's answer LANDS the task.
const LANDED_MODE: &str = "landed";

/// …its words: a `Nothing left:` defense with no need and no open work in
/// Land's strict reading (`turn_end::clean_defense`).
const LANDED_TEXT: &str = "Nothing left: the lab's work is committed and pushed to origin/main.";

/// How long `--land` watches the restored tab: past the 2-minute back-off a
/// loop that attaches to an idle agent waits before its first act, so a land
/// instruction typed for want of the seed is seen.
const LAND_WATCH: Duration = Duration::from_secs(170);

/// The tab's `task=` as its `status` prints it (`?` where it does not).
fn task_of(door: &Door<'_>) -> String {
    status_word(door, "task")
}

/// The land instructions the harness typed into tab `sid` (its ledger's
/// rows of `land@v1` that say `typed`).
fn land_typed(door: &Door<'_>, sid: &str) -> Vec<String> {
    door.harness_ledger(sid)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("land@v1") && l.contains("typed"))
        .map(str::to_string)
        .collect()
}

/// `--land`, before aterm ends: tab `sid` under Land, the stub's answers
/// clean defenses, until its `status` says `task=landed` (6 min). The land
/// instructions typed by then.
fn land_first(run: &Run, door: &Door<'_>, sid: &str) -> Result<Vec<String>, Fail> {
    let spec = J::obj([("stream", J::Bool(true)), ("text", J::s(LANDED_TEXT))]);
    crate::util::atomic_write(
        &run.w.join(format!("modes/{LANDED_MODE}.json")),
        spec.py().as_bytes(),
    )
    .map_err(|e| could_not(&format!("the stub's mode: {e}")))?;
    crate::verbs::set_mode(run, LANDED_MODE)?;
    land_proves_the_journal(&status_word(door, "autopilot"))?;
    let at = format!("@{sid}");
    let said = door
        .ctl(&[&at, "meta", "set", "autopilot", "land"])
        .map_or_else(
            |e| e,
            |r| format!("{}{}", r.stdout, r.stderr).trim().to_string(),
        );
    let t_l = now_ms();
    journal_carries(run, sid, "the Land word", leaf_under_land, t_l)?;
    let mut task = String::new();
    for _ in 0..(6 * 60 * 2) {
        task = task_of(door);
        if task == "landed" {
            break;
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    let typed = land_typed(door, sid);
    println!(
        "  land      tab {sid} under land ({said}): task={task} {} after; {} land instruction(s) typed",
        since(t_l, now_ms()),
        typed.len()
    );
    if task != "landed" {
        return Err(could_not("the tab did not land within 6 min of Land"));
    }
    Ok(typed)
}

/// `--land`, after the restart: the restored tab `sid` watched for
/// [`LAND_WATCH`] — the `task=` values it read, in order, and the land
/// instructions typed since the relaunch (`before`: the old tab's, where the
/// restored tab is the same session).
fn land_after(
    door: &Door<'_>,
    sid: &str,
    old_sid: &str,
    before: &[String],
) -> Result<(Vec<String>, Vec<String>), Fail> {
    let mut seen: Vec<String> = Vec::new();
    let end = now_ms() + i64::try_from(LAND_WATCH.as_millis()).unwrap_or(i64::MAX);
    while now_ms() < end {
        let task = task_of(door);
        if seen.last() != Some(&task) {
            seen.push(task);
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    let mut typed = land_typed(door, sid);
    if sid == old_sid {
        typed.retain(|r| !before.contains(r));
    }
    Ok((seen, typed))
}

/// Which Land run `restore` is (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Land {
    /// `--land`: the tab LANDED before aterm ends.
    Landed,
    /// `--land-open`: the tab is under Land mid-work when aterm ends.
    Open,
}

/// The `HOST` and `DONE` lines of tab `sid`'s journal (the loop's
/// `<sid>.journal.jsonl` under the lab's state, read through no link): the
/// relaunch's carry-on step among them.
fn journal_lines(run: &Run, sid: &str) -> Vec<String> {
    let name = format!("{sid}.journal.jsonl");
    let mut files = Vec::new();
    walk(&run.state(), &mut files);
    files
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == name))
        .flat_map(|p| {
            crate::util::scratch_text(p)
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter_map(|l| {
            let line = J::parse(&l)
                .ok()?
                .get("line")
                .and_then(J::as_str)?
                .to_string();
            (line.starts_with("HOST ") || line.starts_with("DONE ")).then_some(line)
        })
        .collect()
}

/// The relaunch's carry-on step a journal line says, past the words a later
/// carry-on line follows (`HOST seq=… carry-on step=<word>`): `continued`,
/// `done:said-done`, `done:landing`, `failed:<why>`, …. Skipped: a wait, and
/// what the relaunch's own settle is asked again after
/// ([`settles_again`](aterm_agent::harness::relaunch::settles_again):
/// another sweep's lock, `busy:another-sweep` — the real step came 20 s
/// later in the live lab of 2026-10-17). Not its `done:<word>`, which keeps
/// one look open but is the step: the look can only end it `done:lost`.
/// Every other word is the step, fail-closed: a relaunch that failed or was
/// refused stops, and no carry-on line comes after it (`relaunch::owed`), and
/// a word nobody taught the lab is read, never waited past (reviews OW-L1
/// and LAB-1).
fn carry_on_step(line: &str) -> Option<&str> {
    let word = line
        .strip_prefix("HOST ")?
        .split_once(" carry-on step=")?
        .1
        .split_whitespace()
        .next()?;
    let again = word.starts_with("wait:")
        || (aterm_agent::harness::relaunch::settles_again(word) && !word.starts_with("done:"));
    (!again).then_some(word)
}

/// The first carry-on step the journal `lines` say ([`carry_on_step`]), or
/// `None` while every carry-on line is one the host looks at again: not
/// settled.
fn carry_on_settled(lines: &[String]) -> Option<&str> {
    lines.iter().find_map(|l| carry_on_step(l))
}

/// The restored tab's carry-on (its journal `lines`), judged against `want`,
/// the step its Land asks for (`done:landing` mid-work, `done:said-done`
/// once landed): the carry-on SETTLED on `want` ([`carry_on_settled`]: a
/// `want` written after another settled word rescues nothing), no word of it
/// typed Cruise's carry-on (`continued`) or, mid-work, read the work done;
/// and the restored tab is under Land (`autopilot`, its `status`'s word) —
/// which the run made sure no config default gives it
/// ([`land_proves_the_journal`]), so it came back from the layout (review
/// LAB-3). `Err` says what it came to instead.
fn carry_on_judged(lines: &[String], want: &str, autopilot: &str) -> Result<(), String> {
    use aterm_agent::harness::relaunch::CARRIED_DONE;
    let steps: Vec<&str> = lines.iter().filter_map(|l| carry_on_step(l)).collect();
    let settled = carry_on_settled(lines);
    let read_done = want != CARRIED_DONE && steps.contains(&CARRIED_DONE);
    if settled == Some(want) && !steps.contains(&"continued") && !read_done && autopilot == "land" {
        Ok(())
    } else {
        Err(format!(
            "carry-on settled on {} (step(s) {steps:?}), the restored tab autopilot={autopilot}",
            settled.unwrap_or("nothing")
        ))
    }
}

/// `--land-open` and the Cruise controls, after the restart: tab `sid`'s
/// journal until it says the relaunch's carry-on step ([`carry_on_settled`];
/// at most [`LAND_WATCH`]); its `HOST` and `DONE` lines.
fn carry_on_seen(run: &Run, sid: &str) -> Result<Vec<String>, Fail> {
    let end = now_ms() + i64::try_from(LAND_WATCH.as_millis()).unwrap_or(i64::MAX);
    loop {
        let lines = journal_lines(run, sid);
        if carry_on_settled(&lines).is_some() || now_ms() >= end {
            return Ok(lines);
        }
        stop::sleep(Duration::from_millis(500))?;
    }
}

/// The carry-on step of a relaunch under Land whose task is not done
/// (`relaunch::carry_on_once`, F2): nothing of Cruise's typed, the point
/// left to the land policy.
const LAND_RESUMED: &str = "done:landing";

/// The carry-on step of a relaunch under Cruise into open work: Cruise's
/// continuation typed (`relaunch::carry_on_once`). The stub's reply is no
/// done report, so never `continued:check`.
const CRUISE_RESUMED: &str = "continued";

/// The carry-on step a Cruise control (no `--land`, `--land-open` or
/// `--hold`) asks for: [`CRUISE_RESUMED`]; after `--clear`,
/// `done:taskless` — the relaunch resumes the NEW conversation, which
/// nobody has asked anything (its record holds the `/clear` and its empty
/// output alone), and a carry-on there would start a task nobody asked for
/// (`relaunch::nothing_owed`).
fn cruise_step(clear: bool) -> &'static str {
    if clear {
        "done:taskless"
    } else {
        CRUISE_RESUMED
    }
}

/// How many of the upgrade ledger's `rows` say a continuation was typed
/// (`continued`, `continued:check`).
pub(crate) fn continued_typed(rows: &[String]) -> usize {
    rows.iter().filter(|r| r.starts_with("continued")).count()
}

/// A Cruise control's carry-on (the restored tab's journal `lines`), judged
/// against `want` ([`cruise_step`]): the carry-on SETTLED on `want`
/// ([`carry_on_settled`]), no word of it Land's (`done:landing`,
/// `done:said-done`: a tab misreading Land), the ledger's continuation rows
/// (`typed`, [`continued_typed`]) there exactly when `want` types one, and
/// the restored tab under Cruise (`autopilot`, its `status`'s word) — which
/// it was before aterm ended ([`cruise_is_the_control`]). `Err` says what it
/// came to instead.
fn cruise_judged(
    lines: &[String],
    want: &str,
    typed: usize,
    autopilot: &str,
) -> Result<(), String> {
    use aterm_agent::harness::relaunch::CARRIED_DONE;
    let steps: Vec<&str> = lines.iter().filter_map(|l| carry_on_step(l)).collect();
    let settled = carry_on_settled(lines);
    let landed = steps
        .iter()
        .any(|s| [LAND_RESUMED, CARRIED_DONE].contains(s));
    let typed_as_asked = (typed > 0) == want.starts_with(CRUISE_RESUMED);
    if settled == Some(want) && !landed && typed_as_asked && autopilot == "cruise" {
        Ok(())
    } else {
        Err(format!(
            "carry-on settled on {} (step(s) {steps:?}), continuation row(s) typed {typed}, the \
             restored tab autopilot={autopilot}",
            settled.unwrap_or("nothing")
        ))
    }
}

/// Before aterm ends on a Cruise control: the tab's `autopilot` (its
/// `status`'s word) must be Cruise. Under Land or Off — a config default
/// (`LAB_HARNESS_KEYS`) — the control would prove nothing of Cruise's
/// carry-on: COULD NOT RUN.
///
/// # Errors
/// The tab is not under Cruise.
fn cruise_is_the_control(autopilot: &str) -> Result<(), Fail> {
    if autopilot == "cruise" {
        Ok(())
    } else {
        Err(could_not(&format!(
            "the tab is not under Cruise before aterm ends (autopilot={autopilot}; the config \
             default?): the Cruise control would prove nothing of its carry-on"
        )))
    }
}

/// A crash journal's terminal leaf under Land (`autopilot = "land"`).
fn leaf_under_land(leaf: &aterm_toml::Table) -> bool {
    leaf.get("autopilot").and_then(aterm_toml::Value::as_str) == Some("land")
}

/// A crash journal's terminal leaf the owner holds as `--hold` does
/// (`holds` names `local review`).
fn leaf_held(leaf: &aterm_toml::Table) -> bool {
    leaf.get("holds")
        .and_then(aterm_toml::Value::as_array)
        .is_some_and(|holds| holds.iter().any(|h| h.as_str() == Some("local review")))
}

/// Whether the crash journal TOML `text` carries, on session `sid`'s
/// terminal leaf (the table whose `sid = "<sid>"`), what `on_leaf` looks for.
/// Another tab's leaf, or text that is no TOML, carries nothing.
fn leaf_carries(text: &str, sid: &str, on_leaf: fn(&aterm_toml::Table) -> bool) -> bool {
    use aterm_toml::Value;
    fn walk(value: &Value, sid: &str, on_leaf: fn(&aterm_toml::Table) -> bool) -> bool {
        match value {
            Value::Table(table) => {
                (table.get("sid").and_then(Value::as_str) == Some(sid) && on_leaf(table))
                    || table.values().any(|v| walk(v, sid, on_leaf))
            }
            Value::Array(items) => items.iter().any(|v| walk(v, sid, on_leaf)),
            _ => false,
        }
    }
    aterm_toml::from_str::<Value>(text).is_ok_and(|doc| walk(&doc, sid, on_leaf))
}

/// The status word `key=` of the door's tab as its `status` prints it (`?`
/// where it does not).
fn status_word(door: &Door<'_>, key: &str) -> String {
    door.out(&["status"])
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or("?")
        .to_string()
}

/// Before the run puts the tab under Land: its `autopilot` (its `status`'s
/// word) must not be Land already. A config default of Land
/// (`LAB_HARNESS_KEYS`) reopens every tab under Land whatever the layout
/// held, so the restored tab's Land would prove nothing of the crash
/// journal (review LAB-3): COULD NOT RUN.
///
/// # Errors
/// The tab is under Land already.
fn land_proves_the_journal(autopilot: &str) -> Result<(), Fail> {
    if autopilot == "land" {
        return Err(could_not(
            "the tab is under Land before the run sets it (the config default?): the restored \
             tab's Land would prove nothing of the crash journal",
        ));
    }
    Ok(())
}

/// `F s` between two millisecond stamps.
pub(crate) fn since(a: i64, b: i64) -> String {
    // milliseconds as seconds with one decimal, for the report
    #[allow(clippy::cast_precision_loss)]
    let s = (b - a) as f64 / 1000.0;
    format!("{s:.1} s")
}

/// `(pid, sessionId)` of the Claude Code sessions records of a Claude home whose pid is one of `pids`.
#[must_use]
pub fn agents_of(sessions: &Path, pids: &[i32]) -> Vec<(i64, String)> {
    let mut names: Vec<PathBuf> = fs::read_dir(sessions)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    names
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            // each record through no link: the jail writes the Claude home
            let j = J::parse_bytes(&crate::sys::scratch_read(&p).ok()??).ok()?;
            let pid = j.get("pid").and_then(J::as_i64)?;
            let sid = j.get("sessionId").and_then(J::as_str)?.to_string();
            pids.iter()
                .any(|&q| i64::from(q) == pid)
                .then_some((pid, sid))
        })
        .collect()
}

/// The Claude Code under the lab's instance (its sessions record), if any.
pub(crate) fn the_agent(run: &Run) -> Option<(i64, String)> {
    let tree = descendants(run.instance_pid);
    if tree.is_empty() {
        return None;
    }
    agents_of(&run.w.join("home/.claude/sessions"), &tree)
        .into_iter()
        .next()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out),
            Ok(t) if t.is_file() => out.push(p),
            _ => {}
        }
    }
}

/// The quit layout or crash journal under the lab's state that names
/// conversation `conv` (each read through no link: the jail writes the state).
pub(crate) fn desk_naming(run: &Run, conv: &str) -> Option<PathBuf> {
    let mut files = Vec::new();
    walk(&run.state(), &mut files);
    files.into_iter().find(|p| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (name == "session.toml" || name.starts_with("journal-"))
            && crate::sys::scratch_read(p)
                .ok()
                .flatten()
                .is_some_and(|b| String::from_utf8_lossy(&b).contains(conv))
    })
}

/// The upgrade ledger's rows (`step  detail`) written since `ms`: the relaunch's typed line among them.
pub(crate) fn ledger_since(run: &Run, ms: i64) -> Vec<String> {
    let mut files = Vec::new();
    walk(&run.state(), &mut files);
    let since = ms / 1000;
    files
        .iter()
        .filter(|p| {
            p.file_name().is_some_and(|n| n == "ledger.jsonl")
                && p.to_string_lossy().contains("upgrade")
        })
        .flat_map(|p| {
            crate::util::scratch_text(p)
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter_map(|l| {
            let r = J::parse(&l).ok()?;
            (r.get("t").and_then(J::as_i64).unwrap_or(0) >= since).then(|| {
                let s = |k: &str, d: &str| r.get(k).and_then(J::as_str).unwrap_or(d).to_string();
                format!("{}  {}", s("step", "?"), s("detail", ""))
            })
        })
        .collect()
}

/// What `restore` concluded.
pub(crate) enum Verdict {
    Resumed,
    /// `--hold`: the held tab stayed the holder's and its record's lift lifted it.
    Held,
    Failed,
}

/// `%XX` read back as the byte it stands for, as the control socket encodes a
/// free field (every byte that is not ASCII-graphic, and `%` itself); a `%` not
/// followed by two hex digits stays.
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| {
        char::from(c)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The `title=` and the `detail=` lines of the newest `messages` row keyed
/// `key`, decoded.
fn message_keyed(messages: &str, key: &str) -> Option<(String, Vec<String>)> {
    let row = messages
        .lines()
        .rev()
        .find(|row| row.split(' ').any(|w| w == format!("key={key}")))?;
    let field = |name: &str| {
        row.split(' ')
            .find_map(|w| w.strip_prefix(name))
            .map(pct_decode)
            .unwrap_or_default()
    };
    let detail = field("detail=");
    Some((
        field("title="),
        detail.lines().map(str::to_string).collect(),
    ))
}

/// The `aterm ctl` words of the lift a held tab's record names: the
/// command from `aterm ctl ` up to its `off`, as a person would type it.
fn lift_named(lines: &[String]) -> Option<Vec<String>> {
    lines.iter().find_map(|line| {
        let (_, from) = line.split_once("aterm ctl ")?;
        let words: Vec<String> = from.split_whitespace().map(str::to_string).collect();
        let end = words
            .iter()
            .position(|w| w.trim_end_matches([',', '.']) == "off")?;
        Some(
            words[..=end]
                .iter()
                .map(|w| w.trim_end_matches([',', '.']).to_string())
                .collect(),
        )
    })
}

/// The `hold=` of `@<sid> status`, `?` when it says none.
fn hold_of(door: &Door<'_>, sid: &str) -> String {
    door.out(&[&format!("@{sid}"), "status"])
        .split_whitespace()
        .find_map(|w| w.strip_prefix("hold="))
        .unwrap_or("?")
        .to_string()
}

/// `--hold`'s report on the restarted instance: the held tab stays the
/// holder's, its record names a lift the socket takes (module docs). `after`
/// names the Claude Code relaunched (its pid and conversation), if any.
fn held_report(
    run: &Run,
    door: &Door<'_>,
    t_launch: i64,
    after: Option<&(i64, String)>,
) -> Verdict {
    let typed = ledger_since(run, t_launch);
    for row in &typed {
        println!("  typed     {row}");
    }
    match after {
        Some((pid, c)) => {
            println!("  agent     RELAUNCHED: Claude Code pid {pid}, conversation {c}")
        }
        None => println!("  agent     none relaunched within 30 s of the relaunch"),
    }
    let sid = crate::verbs::sid(door);
    let held = hold_of(door, &sid);
    println!("  reopened  tab {sid} hold={held}");
    let record = message_keyed(
        &door.out(&["messages", "tag=harness"]),
        &format!("harness.restored-held.{sid}"),
    );
    let lift = record.as_ref().and_then(|(_, lines)| lift_named(lines));
    match &record {
        Some((title, lines)) => {
            println!("  record    {title}");
            for line in lines {
                println!("    | {line}");
            }
        }
        None => println!("  record    NONE said of the tab"),
    }
    let (lifted, after_lift) = match &lift {
        Some(words) => {
            let args: Vec<&str> = words.iter().map(String::as_str).collect();
            let said = door.ctl(&args).map_or_else(
                |e| e,
                |r| format!("{}{}", r.stdout, r.stderr).trim().to_string(),
            );
            println!(
                "  lift      `aterm ctl {}` run as written: {said}",
                words.join(" ")
            );
            let after_lift = hold_of(door, &sid);
            println!("  status    hold={after_lift}");
            (said.starts_with("OK hold=0"), after_lift)
        }
        None => {
            println!("  lift      the record names NONE");
            (false, String::new())
        }
    };
    if after.is_none() && typed.is_empty() && held == "1" && lifted && after_lift == "0" {
        println!(
            "restore crash --hold: the held tab stayed the holder's; the record's lift lifted it"
        );
        Verdict::Held
    } else {
        println!(
            "restore crash --hold: FAIL: relaunched {}, typed {}, reopened hold={held}, lift {}",
            after.is_some(),
            typed.len(),
            if lifted { "ok" } else { "did not lift" },
        );
        Verdict::Failed
    }
}

fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("restore: COULD NOT RUN: {m}"))
}

/// The aterm under test, once its instance has taken the headless-restore
/// seam (its `aterm.out` says so).
///
/// # Errors
/// The lab records no aterm, or the instance runs without the seam.
pub(crate) fn seam_checked(run: &Run) -> Result<String, Fail> {
    let aterm = run.need_aterm()?.display().to_string();
    if fs::read_to_string(run.run.join("aterm.out"))
        .unwrap_or_default()
        .contains("ATERM_DEBUG_HEADLESS_RESTORE is set")
    {
        Ok(aterm)
    } else {
        Err(could_not(&format!(
            "the instance did not take the headless-restore seam ({aterm}: build it with --features dev-seams)"
        )))
    }
}

/// `--hold`: the owner holds the lab's tab (`hold <sid> on reason=review`),
/// then the crash journal carries the hold ([`journal_carries`]).
///
/// # Errors
/// The journal never carried the hold (FAIL), or interrupted.
pub(crate) fn hold_tab(run: &Run, door: &Door<'_>) -> Result<(), Fail> {
    let sid = crate::verbs::sid(door);
    let said = door
        .ctl(&["hold", &sid, "on", "reason=review"])
        .map_or_else(
            |e| e,
            |r| format!("{}{}", r.stdout, r.stderr).trim().to_string(),
        );
    println!("  hold      tab {sid}: {said}");
    journal_carries(run, &sid, "the owner's hold", leaf_held, now_ms())
}

/// The owner's word on tab `sid`, just said at `t0` (a hold, a Land),
/// reaches the layout the next launch reads before aterm ends: the live
/// instance's own crash journal ([`own_journal_carries`]) carries it on the
/// tab's leaf (`on_leaf`; 30 s, said either way). aterm journals such a word
/// on the loop turn that moved it (`crash_journal::Lane::note_owner_words`);
/// a run that ended aterm without waiting measured the write throttle, not
/// the restore (the live lab of 2026-10-17). One whose journal never carries
/// it has FAILED before any restore, and says so — aterm is not ended, and no
/// restore is judged, so the fault is never pinned on the restore (reviews
/// OW-L2 and LAB-2).
///
/// # Errors
/// The journal never carried the word (FAIL, exit 1), or interrupted.
fn journal_carries(
    run: &Run,
    sid: &str,
    what: &str,
    on_leaf: fn(&aterm_toml::Table) -> bool,
    t0: i64,
) -> Result<(), Fail> {
    journal_carries_within(run, sid, what, on_leaf, t0, 120)
}

/// [`journal_carries`], looking `tries` times 250 ms apart.
fn journal_carries_within(
    run: &Run,
    sid: &str,
    what: &str,
    on_leaf: fn(&aterm_toml::Table) -> bool,
    t0: i64,
    tries: u32,
) -> Result<(), Fail> {
    let mut carried = None;
    for n in 0..tries {
        carried = own_journal_carries(run, sid, on_leaf);
        if carried.is_some() || n + 1 == tries {
            break;
        }
        stop::sleep(Duration::from_millis(250))?;
    }
    let waited = f64::from(tries) / 4.0;
    match carried {
        Some(journal) => {
            println!(
                "  journal   {} carries {what} on tab {sid} within {}",
                journal
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                since(t0, now_ms())
            );
            Ok(())
        }
        None => {
            println!(
                "  journal   NO crash journal of instance {} carried {what} on tab {sid} within {waited:.0} s",
                run.instance_pid
            );
            Err(Fail::exit(
                1,
                format!(
                    "restore: FAIL: the crash journal never carried {what} on tab {sid} within \
                     {waited:.0} s of it; aterm was not ended, and no restore was judged"
                ),
            ))
        }
    }
}

/// The live instance's own crash journal (`journal-<its pid>-<nanos>.toml`
/// under the lab's state, read through no link) that carries `on_leaf` on
/// session `sid`'s leaf ([`leaf_carries`]). Never a quit's `session.toml`,
/// another instance's journal, or another tab's leaf.
fn own_journal_carries(
    run: &Run,
    sid: &str,
    on_leaf: fn(&aterm_toml::Table) -> bool,
) -> Option<PathBuf> {
    let own = format!("journal-{}-", run.instance_pid);
    let mut files = Vec::new();
    walk(&run.state(), &mut files);
    files.into_iter().find(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .is_some_and(|n| n.starts_with(&own) && n.ends_with(".toml"))
            && crate::sys::scratch_read(p)
                .ok()
                .flatten()
                .is_some_and(|b| leaf_carries(&String::from_utf8_lossy(&b), sid, on_leaf))
    })
}

/// aterm ends — `quit` sends `invoke Quit`, `crash` SIGKILLs the instance —
/// and its tree with it (one still alive after 15 s is said and ended: it
/// would hold the conversation); the line of it names what on disk names the
/// conversation `conv`. When it ended.
///
/// # Errors
/// The instance did not end within 30 s, or interrupted.
pub(crate) fn end_aterm(run: &Run, door: &Door<'_>, how: &str, conv: &str) -> Result<i64, Fail> {
    let old_instance = run.instance_pid;
    let old_tree = descendants(run.instance_pid);
    match how {
        "quit" => {
            let _ = door.ctl(&["invoke", "Quit"]);
        }
        _ => {
            let _ = crate::sys::kill(run.instance_pid, libc::SIGKILL);
        }
    }
    let t_end = now_ms();
    if !crate::procs::wait_gone(old_instance, Duration::from_secs(30)) {
        return Err(Fail::exit(
            1,
            format!("restore: FAIL: the instance did not end within 30 s of the {how}"),
        ));
    }
    let t_gone = now_ms();
    // its shell and the agent hang up with the PTYs; one that does not would hold the conversation, so it is said and ended
    let mut left: Vec<i32> = Vec::new();
    for _ in 0..60 {
        left = old_tree.iter().copied().filter(|&p| alive(p)).collect();
        if left.is_empty() {
            break;
        }
        stop::sleep(Duration::from_millis(250))?;
    }
    let left_s: String = left.iter().map(|p| format!(" {p}")).collect();
    if !left.is_empty() {
        eprintln!("aterm-lab: still alive 15 s after aterm ended:{left_s} (ended by pid)");
        for &p in &left {
            let _ = crate::sys::kill(p, libc::SIGKILL);
        }
    }
    let layout = desk_naming(run, conv).map_or_else(
        || "NOTHING on disk names the conversation".to_string(),
        |p| {
            format!(
                "{} names the conversation",
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )
        },
    );
    println!(
        "  end       {how}: instance {old_instance} gone after {}; its tree{} gone after {}; {layout}",
        since(t_end, t_gone),
        if left.is_empty() {
            String::new()
        } else {
            format!(" (but{left_s})")
        },
        since(t_end, now_ms())
    );
    Ok(t_end)
}

/// The next launch on the same state and socket, `t_end` the end's stamp. The
/// jail denies unlinking the socket and its token, so no jailed instance can
/// clear what the last one left: the lab removes both, from outside. When the
/// launch began and when its socket came up.
///
/// # Errors
/// Not a lab socket directory, a link at the socket or its token, or the
/// instance did not come up.
pub(crate) fn relaunch_instance(
    run: &mut Run,
    how: &str,
    armed: bool,
    t_end: i64,
) -> Result<(i64, i64), Fail> {
    if !run.sockdir.starts_with("/tmp/at.") {
        return Err(could_not(&format!("not a lab socket dir: {}", run.sockdir)));
    }
    let link = |p: &str| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
    if link(&run.sock) || link(&format!("{}/a.sock.token", run.sockdir)) {
        return Err(could_not(&format!(
            "a link stands at {} or its token",
            run.sock
        )));
    }
    let _ = fs::remove_file(&run.sock);
    let _ = fs::remove_file(format!("{}/a.sock.token", run.sockdir));
    let t_launch = now_ms();
    if let Err(e) = crate::lab::start_instance(run, UpOpts::from_env().scale.as_deref(), armed) {
        let out = fs::read_to_string(run.run.join("aterm.out")).unwrap_or_default();
        let tail: Vec<&str> = out.lines().rev().take(20).collect();
        for l in tail.iter().rev() {
            eprintln!("{l}");
        }
        return Err(Fail::exit(
            1,
            format!("restore: FAIL: the relaunched instance did not come up ({e})"),
        ));
    }
    let t_up = now_ms();
    println!(
        "  relaunch  instance {} up in {} ({} after the {how})",
        run.instance_pid,
        since(t_launch, t_up),
        since(t_end, t_up)
    );
    Ok((t_launch, t_up))
}

/// One restore run on the lab `tag`, which it brought up.
#[allow(clippy::too_many_lines)]
fn run_restore(
    tag: &str,
    how: &str,
    armed: bool,
    hold: bool,
    clear: bool,
    land: Option<Land>,
) -> Result<Verdict, Fail> {
    let mut run = Run::load(tag)?;
    let aterm = seam_checked(&run)?;
    println!(
        "restore {how}{}{}{}: lab '{tag}', Claude Code {}, {aterm}",
        if clear { " --clear" } else { "" },
        if hold { " --hold" } else { "" },
        match land {
            Some(Land::Landed) => " --land",
            Some(Land::Open) => " --land-open",
            None => "",
        },
        run.claude_ver
    );

    // ---- Claude Code in the first tab, one turn, and the conversation it registered
    crate::lab::launch_claude(
        &run,
        &Launch {
            no_wait: true,
            ..Launch::default()
        },
    )
    .map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            could_not(&format!(
                "Claude Code could not be launched: {}",
                f.msg.unwrap_or_default()
            ))
        }
    })?;
    let door = Door::new(&run)?;
    if !door.seen("❯", 60)? {
        return Err(could_not("Claude Code never drew its composer"));
    }
    crate::verbs::turn(&door, "hi")?;
    if !door.seen("I am the local lab stub", 60)? {
        return Err(could_not("the stub's answer never showed"));
    }
    let mut before = None;
    for _ in 0..60 {
        before = the_agent(&run);
        if before.is_some() {
            break;
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    let Some((old_pid, conv)) = before else {
        return Err(could_not(
            "Claude Code registered no conversation (no sessions/<pid>.json in its home)",
        ));
    };
    let t0 = now_ms();
    // the desk names the agent: the crash journal's image (a quit's layout names the agents the same way, at the quit)
    let mut named = None;
    for _ in 0..120 {
        named = desk_naming(&run, &conv);
        if named.is_some() {
            break;
        }
        stop::sleep(Duration::from_millis(250))?;
    }
    println!(
        "  before    Claude Code pid {old_pid}, conversation {conv}; {}",
        if named.is_some() {
            format!("the crash journal named it within {}", since(t0, now_ms()))
        } else {
            "NO crash journal named it within 30 s".into()
        }
    );

    // ---- `/clear` in the app (LC-1 of ATERM-KI-1015-1): Claude Code's record
    // names a new conversation within a second, and the desk must name it too
    let conv = if clear {
        let t_c = now_ms();
        if !crate::verbs::turn(&door, "/clear")? {
            return Err(could_not("the composer did not take /clear"));
        }
        let mut cleared = None;
        for _ in 0..120 {
            if let Some((_, c)) = the_agent(&run)
                && c != conv
            {
                cleared = Some(c);
                break;
            }
            stop::sleep(Duration::from_millis(250))?;
        }
        let Some(cleared) = cleared else {
            return Err(could_not(
                "Claude Code's record named no new conversation within 30 s of /clear",
            ));
        };
        let t_rec = now_ms();
        let mut named = None;
        for _ in 0..120 {
            named = desk_naming(&run, &cleared);
            if named.is_some() {
                break;
            }
            stop::sleep(Duration::from_millis(250))?;
        }
        println!(
            "  clear     the record names conversation {cleared} {} after /clear; {}",
            since(t_c, t_rec),
            if named.is_some() {
                format!(
                    "the crash journal named it {} after /clear",
                    since(t_c, now_ms())
                )
            } else {
                "NO crash journal named it within 30 s".into()
            }
        );
        cleared
    } else {
        conv
    };

    // ---- the owner holds the tab, and the crash journal carries the hold
    if hold {
        hold_tab(&run, &door)?;
    }

    // ---- `--land`: the tab LANDED before aterm ends
    let old_sid = crate::verbs::sid(&door);
    let landed_before = match land {
        Some(Land::Landed) => land_first(&run, &door, &old_sid)?,
        Some(Land::Open) => {
            // mid-work: the stub's ordinary reply on screen, and aterm ends
            // before Land's back-off could type anything
            land_proves_the_journal(&status_word(&door, "autopilot"))?;
            let said = door
                .ctl(&[&format!("@{old_sid}"), "meta", "set", "autopilot", "land"])
                .map_or_else(
                    |e| e,
                    |r| format!("{}{}", r.stdout, r.stderr).trim().to_string(),
                );
            println!(
                "  land      tab {old_sid} under land mid-work ({said}): task={}",
                task_of(&door)
            );
            journal_carries(&run, &old_sid, "the Land word", leaf_under_land, now_ms())?;
            Vec::new()
        }
        // a Cruise control: its tab under Cruise, or it controls nothing
        None if !hold => {
            cruise_is_the_control(&status_word(&door, "autopilot"))?;
            Vec::new()
        }
        None => Vec::new(),
    };

    // ---- aterm ends, and comes back on the same state and socket
    let t_end = end_aterm(&run, &door, how, &conv)?;
    let (t_launch, t_up) = relaunch_instance(&mut run, how, armed, t_end)?;

    // ---- the restored tab: the harness types the relaunch and Claude Code comes back, to its conversation or not
    let door = Door::new(&run)?;
    let (mut after, mut t_agent) = (None, 0);
    // a held tab's agent must not come back: 30 s is past every relaunch measured
    for _ in 0..if hold { 120 } else { 360 } {
        after = the_agent(&run);
        if after.is_some() {
            t_agent = now_ms();
            break;
        }
        stop::sleep(Duration::from_millis(250))?;
    }
    if hold {
        return Ok(held_report(&run, &door, t_launch, after.as_ref()));
    }
    let ready = "❯|Choose the text style|Select login method|Welcome to Claude";
    door.seen(ready, 30)?;
    let screen = door.out(&["text"]);
    let typed_early = ledger_since(&run, t_launch);
    for row in &typed_early {
        println!("  typed     {row}");
    }
    let onboarding = [
        "Choose the text style",
        "Select login method",
        "Welcome to Claude Code",
    ]
    .iter()
    .any(|n| screen.contains(n))
        || screen.contains("Let's get started")
        || screen.contains("Let\u{2019}s get started");
    let reply = screen.contains("I am the local lab stub");
    let resumed = after.as_ref().is_some_and(|(_, c)| *c == conv);
    match &after {
        Some((pid, c)) => println!(
            "  agent     Claude Code pid {pid}, conversation {c}, registered {} after the relaunch came up ({} after the {how})",
            since(t_up, t_agent),
            since(t_end, t_agent)
        ),
        None => println!("  agent     NONE registered within 90 s of the relaunch"),
    }
    println!(
        "  screen    {}; the earlier reply on it: {}",
        if onboarding {
            "ONBOARDING (Claude Code's first run)"
        } else {
            "no onboarding"
        },
        if reply { "yes" } else { "no" }
    );
    let rows: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    for l in &rows[rows.len().saturating_sub(6)..] {
        println!("    | {}", l.chars().take(140).collect::<String>());
    }
    if let Some(land) = land {
        let sid = crate::verbs::sid(&door);
        match land {
            Land::Landed => {
                let (seen, typed) = land_after(&door, &sid, &old_sid, &landed_before)?;
                let journal = journal_lines(&run, &sid);
                let rows = ledger_since(&run, t_launch);
                println!(
                    "  land      tab {sid} (before: {old_sid}) read task={} over {} s; land instruction(s) typed since the relaunch: {}",
                    seen.join("→"),
                    LAND_WATCH.as_secs(),
                    typed.len()
                );
                for row in &typed {
                    println!("    | {row}");
                }
                for line in &journal {
                    println!("  journal   {line}");
                }
                println!("  status    {}", door.out(&["status"]).trim());
                let carry_on = carry_on_judged(
                    &journal,
                    aterm_agent::harness::relaunch::CARRIED_DONE,
                    &status_word(&door, "autopilot"),
                );
                let task = seen.last().map_or("?", String::as_str);
                if task != "landed"
                    || !typed.is_empty()
                    || carry_on.is_err()
                    || continued_typed(&rows) > 0
                {
                    println!(
                        "restore {how} --land: FAIL: task={task}, land instruction(s) typed {}, {}, carry-on line(s) typed {}",
                        typed.len(),
                        carry_on
                            .err()
                            .unwrap_or_else(|| "the carry-on as asked".into()),
                        continued_typed(&rows)
                    );
                    return Ok(Verdict::Failed);
                }
                println!(
                    "restore {how} --land: the finished task stayed {task}, nothing typed into it"
                );
            }
            Land::Open => {
                let journal = carry_on_seen(&run, &sid)?;
                let rows = ledger_since(&run, t_launch);
                for line in &journal {
                    println!("  journal   {line}");
                }
                println!("  status    {}", door.out(&["status"]).trim());
                let carry_on =
                    carry_on_judged(&journal, LAND_RESUMED, &status_word(&door, "autopilot"));
                if carry_on.is_err() || continued_typed(&rows) > 0 {
                    println!(
                        "restore {how} --land-open: FAIL: {}, carry-on line(s) typed {}",
                        carry_on
                            .err()
                            .unwrap_or_else(|| "the carry-on as asked".into()),
                        continued_typed(&rows)
                    );
                    return Ok(Verdict::Failed);
                }
                println!(
                    "restore {how} --land-open: the work under Land was not read as done, and no carry-on of Cruise's was typed"
                );
            }
        }
    } else if resumed && !onboarding {
        // A Cruise control: the relaunch's carry-on settles a few seconds
        // after the agent is back (its first idle point), and is judged as
        // the Land runs judge theirs — a tab that misread Land or Off
        // would otherwise pass on its resume alone.
        let sid = crate::verbs::sid(&door);
        let journal = carry_on_seen(&run, &sid)?;
        let rows = ledger_since(&run, t_launch);
        for row in rows.iter().skip(typed_early.len()) {
            println!("  typed     {row}");
        }
        for line in &journal {
            println!("  journal   {line}");
        }
        let want = cruise_step(clear);
        let mode = if clear { " --clear" } else { "" };
        if let Err(why) = cruise_judged(
            &journal,
            want,
            continued_typed(&rows),
            &status_word(&door, "autopilot"),
        ) {
            println!("  status    {}", door.out(&["status"]).trim());
            println!("restore {how}{mode}: FAIL: {why}");
            return Ok(Verdict::Failed);
        }
        println!("restore {how}{mode}: the relaunch's carry-on settled on {want}, under Cruise");
    }
    if resumed && !onboarding {
        println!("restore {how}: RESUMED the same conversation");
        Ok(Verdict::Resumed)
    } else {
        println!(
            "restore {how}: FAIL: {}{}",
            after.map_or_else(
                || "no agent came back".to_string(),
                |(_, c)| format!("the restored agent is on conversation {c}, not {conv}")
            ),
            if onboarding { ", in onboarding" } else { "" }
        );
        Ok(Verdict::Failed)
    }
}

/// What `restore`'s words ask for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Asked {
    /// The lab's tag, when one was given.
    tag: Option<String>,
    /// `quit` or `crash`.
    how: String,
    keep: bool,
    hold: bool,
    clear: bool,
    land: Option<Land>,
}

/// `restore`'s words read; `Ok(None)` for `-h`/`--help`.
///
/// # Errors
/// An unknown option, no `quit`/`crash`, or options that do not go together.
fn parse_args(args: &[String]) -> Result<Option<Asked>, Fail> {
    let usage = |m: &str| Fail::usage(format!("restore: {m}"));
    let mut a = Asked::default();
    let mut how = None::<String>;
    for w in args {
        match w.as_str() {
            "quit" | "crash" => how = Some(w.clone()),
            "--keep" => a.keep = true,
            "--hold" => a.hold = true,
            "--clear" => a.clear = true,
            "--land" => a.land = Some(Land::Landed),
            "--land-open" => a.land = Some(Land::Open),
            "-h" | "--help" => return Ok(None),
            s if s.starts_with('-') => {
                return Err(usage(&format!(
                    "unknown option '{s}' (quit|crash, --keep, --hold, --clear, --land, --land-open)"
                )));
            }
            s => a.tag = Some(s.to_string()),
        }
    }
    a.how = how.ok_or_else(|| usage("say how aterm ends: quit or crash"))?;
    if a.hold && a.how != "crash" {
        return Err(usage(
            "--hold takes crash: a held tab refuses `invoke Quit` (ERR halted)",
        ));
    }
    if a.land.is_some() && a.hold {
        return Err(usage(
            "--land and --land-open take no --hold: a held tab's agent is not relaunched",
        ));
    }
    // A cleared conversation is taskless: its relaunch settles on
    // `done:taskless` whatever the tab's Autopilot (`relaunch::nothing_owed`
    // is asked before Land), so a Land run on it judges a landing nothing
    // can reach.
    if a.clear && a.land.is_some() {
        return Err(usage(
            "--clear takes no --land or --land-open: a cleared conversation is taskless, so its \
             relaunch types nothing whatever the Autopilot and Land has no work to land or leave open",
        ));
    }
    Ok(Some(a))
}

/// `restore [tag] quit|crash [--keep] [--hold] [--clear] [--land|--land-open]`.
pub fn cmd_restore(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("restore: {m}"));
    let Some(Asked {
        tag,
        how,
        keep,
        hold,
        clear,
        land,
    }) = parse_args(args)?
    else {
        print!("{}", crate::USAGE);
        return Ok(0);
    };
    let tag = tag.unwrap_or_else(|| format!("restore-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: restore brings up a lab of its own (another tag, or {LAB} down {tag})"
        )));
    }
    stop::install();
    // the seam, and the folder trust seeded: the restored agent must come back to its conversation, not to a dialog
    let mut u = UpOpts::from_env();
    u.restore = true;
    u.trust = true;
    // a lab of its own ends with this run, unless it is kept
    u.lifeline = !keep;
    if let Err(f) = crate::lab::up(&tag, &u) {
        if let Some(m) = &f.msg {
            eprintln!("aterm-lab: {m}");
        }
        if f.stopped.is_some() {
            return Err(f);
        }
        eprintln!("aterm-lab: restore: COULD NOT RUN: the lab did not come up");
        return Ok(3);
    }
    let verdict = run_restore(&tag, &how, !keep, hold, clear, land);
    if keep {
        println!("restore: lab '{tag}' left up ({LAB} down {tag})");
    } else {
        let _ = crate::lab::down(&tag, false);
    }
    match verdict? {
        Verdict::Resumed | Verdict::Held => Ok(0),
        Verdict::Failed => Ok(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_is_the_sessions_record_whose_pid_is_in_the_tree() {
        // a physical path, as the lab's state dir is: a record is read through no link on its whole path
        let dir = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("lab-sessions-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("100.json"), r#"{"pid":100,"sessionId":"conv-a"}"#).unwrap();
        fs::write(dir.join("200.json"), r#"{"pid":200,"sessionId":"conv-b"}"#).unwrap();
        fs::write(dir.join("notes.txt"), "x").unwrap();
        assert_eq!(
            agents_of(&dir, &[200, 300]),
            vec![(200, "conv-b".to_string())]
        );
        assert!(agents_of(&dir, &[1]).is_empty());
        // through a link anywhere on the path, nothing is read
        let link = dir.with_extension("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(agents_of(&link, &[200]).is_empty());
        let _ = fs::remove_file(&link);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(since(1000, 3350), "2.4 s");
    }

    /// `--land` and `--land-open` read the relaunch's carry-on step off the
    /// restored tab's journal lines: the word past its waits, of a HOST line.
    #[test]
    fn the_carry_on_step_is_read_off_a_host_line() {
        assert_eq!(
            carry_on_step("HOST seq=12 carry-on step=done:said-done"),
            Some(aterm_agent::harness::relaunch::CARRIED_DONE)
        );
        assert_eq!(
            carry_on_step("HOST seq=3 carry-on step=continued"),
            Some("continued")
        );
        assert_eq!(
            carry_on_step("HOST seq=5 carry-on step=done:landing"),
            Some(LAND_RESUMED)
        );
        // its waits, another host step and a policy line say no step
        assert_eq!(carry_on_step("HOST seq=3 carry-on step=wait:held"), None);
        assert_eq!(carry_on_step("HOST seq=3 upgrade step=announced:1"), None);
        assert_eq!(carry_on_step("DONE seq=4 carry-on step=continued"), None);
    }

    /// THE CARRY-ON SETTLES ON ITS LAST WORD, NOT ITS FIRST (the live lab of
    /// 2026-10-17, `restore crash --land-open`): the restored tab's first
    /// carry-on line said `busy:another-sweep` — another sweep held the
    /// lock, and the host settled again (`relaunch::settles_again`) — and the
    /// real step, `done:landing`, came 20 s later; the run read the first
    /// line and failed. A lone word the host looks again after is not
    /// settled, whichever it is. NEGATIVE CONTROL: a state directory that
    /// cannot hold the lock (`busy:state-unwritable`) is a last word, read.
    #[test]
    fn the_carry_on_settles_past_another_sweeps_lock() {
        let lines = |words: &[&str]| -> Vec<String> {
            words
                .iter()
                .enumerate()
                .map(|(seq, word)| format!("HOST seq={seq} carry-on step={word}"))
                .collect()
        };
        assert_eq!(
            carry_on_settled(&lines(&["busy:another-sweep", "done:landing"])),
            Some(LAND_RESUMED)
        );
        assert_eq!(
            carry_on_settled(&lines(&["wait:resume", "busy:another-sweep", "continued"])),
            Some("continued")
        );
        for lone in ["busy:another-sweep", "wait:held", "wait:restore"] {
            assert_eq!(carry_on_step(&lines(&[lone])[0]), None, "{lone}");
            assert_eq!(
                carry_on_settled(&lines(&[lone])),
                None,
                "{lone}: not settled"
            );
        }
        assert_eq!(
            carry_on_settled(&lines(&["busy:state-unwritable"])),
            Some("busy:state-unwritable")
        );
        assert_eq!(carry_on_settled(&[]), None);
    }

    /// A CARRY-ON SETTLES ON EVERY WORD NO LATER CARRY-ON LINE FOLLOWS
    /// (reviews OW-L1 and LAB-1 of the owner-word fix): only a wait and
    /// another sweep's lock are settled again later (`relaunch::settles_again`).
    /// A relaunch that failed or was refused stops (`st.stop`: `relaunch::owed`
    /// goes false, and the host journals no other carry-on line), and a word
    /// the host was never taught is no wait. Each is the step: the run
    /// settles on it and fails naming it — never skipped past to a
    /// `done:landing` written after it, which would pass.
    #[test]
    fn a_failed_refused_or_untaught_carry_on_is_the_step() {
        let lines = |words: &[&str]| -> Vec<String> {
            words
                .iter()
                .enumerate()
                .map(|(seq, word)| format!("HOST seq={seq} carry-on step={word}"))
                .collect()
        };
        for word in [
            "failed:no-resume",
            "failed:exited-before-continuing",
            "failed:signal-refused",
            "refused:one-shot",
            "refused:shell-gone",
            "typed:a-word-never-taught",
        ] {
            assert_eq!(carry_on_step(&lines(&[word])[0]), Some(word), "{word}");
            assert_eq!(
                carry_on_settled(&lines(&["busy:another-sweep", word, LAND_RESUMED])),
                Some(word),
                "{word}: the step, not a later done:landing"
            );
        }
    }

    /// THE VERDICT IS THE SETTLED WORD, ON A TAB THE LAYOUT PUT UNDER LAND
    /// (reviews LAB-1 and LAB-3): `--land-open` passes only where the
    /// carry-on settled on `done:landing` and the restored tab reads
    /// `autopilot=land`; `--land` only on `done:said-done`. NEGATIVE
    /// CONTROLS: a `done:landing` after a failed relaunch or an untaught word
    /// rescues nothing, nor a later `continued` or done read; a restored tab
    /// that is not under Land fails however its carry-on settled; and a tab
    /// under Land before the run sets it (a config default) cannot run.
    #[test]
    fn the_land_verdicts_read_the_settled_word_on_a_tab_under_land() {
        use aterm_agent::harness::relaunch::CARRIED_DONE;
        let lines = |words: &[&str]| -> Vec<String> {
            words
                .iter()
                .enumerate()
                .map(|(seq, word)| format!("HOST seq={seq} carry-on step={word}"))
                .collect()
        };
        let open = |words: &[&str], autopilot: &str| {
            carry_on_judged(&lines(words), LAND_RESUMED, autopilot)
        };
        assert_eq!(open(&["busy:another-sweep", LAND_RESUMED], "land"), Ok(()));
        assert_eq!(
            carry_on_judged(&lines(&["wait:resume", CARRIED_DONE]), CARRIED_DONE, "land"),
            Ok(())
        );
        for wrong in [
            &["failed:no-resume", LAND_RESUMED][..],
            &["typed:a-word-never-taught", LAND_RESUMED],
            &["continued", LAND_RESUMED],
            &[LAND_RESUMED, "continued"],
            &[LAND_RESUMED, CARRIED_DONE],
            &[CARRIED_DONE],
            &["busy:another-sweep"],
            &[],
        ] {
            let said = open(wrong, "land").expect_err(&format!("{wrong:?}"));
            assert!(said.contains("carry-on settled on"), "{said}");
        }
        assert!(open(&[LAND_RESUMED], "cruise").is_err(), "not under Land");
        assert!(open(&[LAND_RESUMED], "?").is_err(), "no status");
        assert!(
            carry_on_judged(&lines(&[LAND_RESUMED]), CARRIED_DONE, "land").is_err(),
            "--land settles on done:said-done"
        );
        assert_eq!(land_proves_the_journal("land").map_err(|f| f.code), Err(3));
        for before in ["cruise", "off", "?"] {
            assert!(land_proves_the_journal(before).is_ok(), "{before}");
        }
    }

    /// THE CRUISE CONTROLS JUDGE THEIR CARRY-ON TOO (the live lab of
    /// 2026-10-17): a plain `restore crash` or `quit` passes only where the
    /// restored tab's carry-on SETTLED on `continued` — Cruise's carry-on
    /// typed into open work, the ledger's `continued` row with it — on a tab
    /// under Cruise; `--clear` only on `done:taskless` (nobody asked the new
    /// conversation anything), nothing typed. NEGATIVE CONTROLS: a lone
    /// `done:landing` (a tab misreading Land) or `done:said-done` fails, so
    /// do a Land word anywhere among the steps, a `continued` after a failed
    /// relaunch, a carry-on that never settled, a journal's `continued` the
    /// ledger never typed, and a restored tab not under Cruise; a tab not
    /// under Cruise before the run ends aterm cannot run.
    #[test]
    fn the_cruise_controls_read_a_settled_continued() {
        use aterm_agent::harness::relaunch::CARRIED_DONE;
        let lines = |words: &[&str]| -> Vec<String> {
            words
                .iter()
                .enumerate()
                .map(|(seq, word)| format!("HOST seq={seq} carry-on step={word}"))
                .collect()
        };
        let plain = cruise_step(false);
        let cleared = cruise_step(true);
        assert_eq!((plain, cleared), (CRUISE_RESUMED, "done:taskless"));
        let cruise =
            |words: &[&str], typed: usize| cruise_judged(&lines(words), plain, typed, "cruise");
        assert_eq!(cruise(&["continued", "done"], 1), Ok(()));
        assert_eq!(cruise(&["busy:another-sweep", "continued"], 1), Ok(()));
        for (wrong, typed) in [
            (&[LAND_RESUMED][..], 0),
            (&[CARRIED_DONE], 0),
            (&["busy:another-sweep", LAND_RESUMED], 0),
            (&["continued", LAND_RESUMED], 1),
            (&["done:taskless"], 0),
            (&["failed:no-resume", "continued"], 1),
            (&["busy:another-sweep"], 0),
            (&[], 0),
            (&["continued"], 0),
        ] {
            let said = cruise(wrong, typed).expect_err(&format!("{wrong:?} typed {typed}"));
            assert!(said.contains("carry-on settled on"), "{said}");
        }
        assert!(
            cruise_judged(&lines(&["continued"]), plain, 1, "land").is_err(),
            "not under Cruise"
        );
        assert!(
            cruise_judged(&lines(&["continued"]), plain, 1, "?").is_err(),
            "no status"
        );
        assert_eq!(
            cruise_judged(
                &lines(&["done:taskless", "done:lost"]),
                cleared,
                0,
                "cruise"
            ),
            Ok(())
        );
        assert!(cruise_judged(&lines(&["continued"]), cleared, 1, "cruise").is_err());
        assert!(cruise_judged(&lines(&["done:taskless"]), cleared, 1, "cruise").is_err());
        assert_eq!(cruise_is_the_control("land").map_err(|f| f.code), Err(3));
        assert_eq!(cruise_is_the_control("off").map_err(|f| f.code), Err(3));
        assert_eq!(cruise_is_the_control("?").map_err(|f| f.code), Err(3));
        assert!(cruise_is_the_control("cruise").is_ok());
        assert_eq!(
            continued_typed(&[
                "relaunched  x".to_string(),
                "continued  the continuation is typed".to_string(),
                "continued:check  y".to_string(),
                "done  z".to_string(),
            ]),
            2
        );
    }

    /// THE OWNER'S WORD IS LOOKED FOR WHERE THE NEXT LAUNCH READS IT, AND ITS
    /// ABSENCE FAILS THE RUN BEFORE ANY RESTORE (reviews OW-L2 and LAB-2):
    /// the live instance's own crash journal, on the tab's own leaf. A
    /// journal there carrying Land or the hold on the tab's leaf is found;
    /// NEGATIVE CONTROLS: the word on another tab's leaf, in a quit's
    /// `session.toml`, or in another instance's journal is not, and the wait
    /// then ends in a FAIL (exit 1) that names the journal.
    #[test]
    fn the_owners_word_is_found_on_the_tabs_leaf_of_the_live_journal_or_fails() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("lab-owner-word-")
            .tempdir()
            .expect("exclusive scratch");
        let root = scratch.path().canonicalize().expect("physical scratch");
        let run = Run {
            w: root.clone(),
            instance_pid: 4242,
            ..Run::default()
        };
        let state = run.state().join("aterm");
        fs::create_dir_all(&state).unwrap();
        let journal = |sid: &str, words: &str| {
            format!(
                "schema = 2\n\n[[windows]]\nrows = 50\ncols = 144\n\n[[windows.restored_tabs]]\n\
                 zoomed = false\n\n[windows.restored_tabs.root]\nnode = \"leaf\"\n\n\
                 [windows.restored_tabs.root.view]\nkind = \"terminal\"\nlocal_id = 0\n\
                 sid = \"{sid}\"\n{words}\n\n[windows.restored_tabs.root.view.agent]\n\
                 tab = \"s-tab\"\n\n[journal]\npid = 4242\n"
            )
        };
        let land = "autopilot = \"land\"";
        let held = "holds = [\"local review\"]";
        let found =
            |on_leaf| journal_carries_within(&run, "s-tab", "the word", on_leaf, now_ms(), 1);
        let failed = |r: Result<(), Fail>| {
            let f = r.expect_err("not carried");
            assert_eq!(f.code, 1);
            assert!(
                f.msg
                    .unwrap_or_default()
                    .contains("FAIL: the crash journal never carried the word on tab s-tab"),
            );
        };

        // NEGATIVE CONTROLS: nowhere the next launch reads it for this tab.
        fs::write(state.join("session.toml"), journal("s-tab", land)).unwrap();
        fs::write(state.join("journal-99-1.toml"), journal("s-tab", land)).unwrap();
        fs::write(state.join("journal-4242-1.toml"), journal("s-other", land)).unwrap();
        failed(found(leaf_under_land));
        assert!(!leaf_carries(&journal("s-tab", land), "s-tab", leaf_held));
        assert!(!leaf_carries("no toml [", "s-tab", leaf_under_land));

        // On the tab's leaf of the live instance's journal: found.
        fs::write(
            state.join("journal-4242-1.toml"),
            journal("s-tab", &format!("{land}\n{held}")),
        )
        .unwrap();
        assert!(found(leaf_under_land).is_ok());
        assert!(found(leaf_held).is_ok());
        assert!(own_journal_carries(&run, "s-tab", leaf_held).is_some());
    }

    /// The words every restore takes: Claude Code's `--clear` and `--land`,
    /// and the rules each keeps (`--hold` takes `crash`). NEGATIVE CONTROLS:
    /// the retired `--codex` (a vendor Codex is restored by no lane) is an
    /// unknown option, and so is no `quit` or `crash`.
    #[test]
    fn a_restore_takes_claude_codes_own_drives_and_refuses_the_retired_codex() {
        let words = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        let asked = |s: &str| parse_args(&words(s)).map_err(|f| (f.code, f.msg));
        assert!(asked("crash --clear").unwrap().unwrap().clear);
        assert_eq!(
            asked("quit --land").unwrap().unwrap().land,
            Some(Land::Landed)
        );
        assert_eq!(
            asked("quit --land-open").unwrap().unwrap().land,
            Some(Land::Open)
        );
        assert!(asked("t crash --hold").unwrap().unwrap().hold);
        assert!(asked("quit --hold").is_err(), "--hold takes crash");
        let (code, msg) = asked("crash --codex").unwrap_err();
        assert_eq!(code, 2);
        assert!(msg.unwrap_or_default().contains("unknown option '--codex'"));
        assert!(asked("--keep").is_err(), "no quit or crash");
        assert_eq!(asked("--help").unwrap(), None);
    }

    /// `--clear` takes neither Land run (the review's RV2-LAB-1): a cleared
    /// conversation is taskless, so its relaunch settles on `done:taskless`
    /// whatever the tab's Autopilot, and `--land` or `--land-open` would judge
    /// a landing nothing can reach. Refused as usage, in either order.
    /// NEGATIVE CONTROLS: `--clear` alone, each Land run alone, and `--clear`
    /// with `--hold` are taken.
    #[test]
    fn a_cleared_conversation_takes_no_land_run() {
        let words = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        let asked = |s: &str| parse_args(&words(s)).map_err(|f| (f.code, f.msg));
        for refused in [
            "crash --clear --land",
            "quit --clear --land",
            "crash --clear --land-open",
            "quit --land-open --clear",
        ] {
            let (code, msg) = asked(refused).unwrap_err();
            assert_eq!(code, 2, "{refused}");
            assert!(
                msg.unwrap_or_default()
                    .contains("--clear takes no --land or --land-open"),
                "{refused}"
            );
        }
        assert!(asked("quit --clear").unwrap().unwrap().clear);
        let held = asked("crash --clear --hold").unwrap().unwrap();
        assert!(held.clear && held.hold);
        assert_eq!(
            asked("crash --land-open").unwrap().unwrap().land,
            Some(Land::Open)
        );
        assert_eq!(
            asked("crash --land").unwrap().unwrap().land,
            Some(Land::Landed)
        );
    }

    /// `--hold` reads the lift off the record as the socket prints it: the
    /// newest row of the key, its detail decoded, the command up to its `off`.
    #[test]
    fn the_lift_is_read_off_the_held_tabs_record() {
        let messages = "message 3 at=1 ago_ms=1 tag=harness sev=info origin=host state=recorded \
                        glass=- rep=1 key=harness.restored-held.s-1 title=Tab%201:%20old detail=x \
                        actions=-\n\
                        message 7 at=2 ago_ms=1 tag=harness sev=info origin=host state=recorded \
                        glass=- rep=1 key=harness.restored-held.s-1 \
                        title=Tab%201:%20Claude%20was%20not%20restarted \
                        detail=To%20pick%20it%20up:%0Aclaude%20--resume%20c-1%0AYour%20hold%20came\
                        %20back:%20lift%20it%20with%20the%20menu,%20or%20aterm%20ctl%20hold%20s-1\
                        %20off,%20when%20drivers%20may%20type%20there%20again%25 actions=-";
        let (title, lines) = message_keyed(messages, "harness.restored-held.s-1").unwrap();
        assert_eq!(
            title, "Tab 1: Claude was not restarted",
            "the newest row of the key"
        );
        assert_eq!(lines[1], "claude --resume c-1");
        assert!(
            lines[2].ends_with("again%"),
            "a decoded %25: {:?}",
            lines[2]
        );
        assert_eq!(
            lift_named(&lines),
            Some(vec![
                "hold".to_string(),
                "s-1".to_string(),
                "off".to_string()
            ])
        );
        assert_eq!(message_keyed(messages, "harness.restored-held.s-2"), None);
        assert_eq!(lift_named(&["lift it with the menu".to_string()]), None);
        assert_eq!(pct_decode("%4"), "%4", "a % without two hex digits stays");
    }

    /// `--land`'s stub answer LANDS the work under Land's own reading: a
    /// clean defense. NEGATIVE CONTROL: the retired token reply it once gave
    /// lands nothing.
    #[test]
    fn the_land_drills_stub_answer_is_a_clean_defense() {
        use aterm_agent::supervise::policy::turn_end::clean_defense;
        assert!(clean_defense(Some(LANDED_TEXT)), "{LANDED_TEXT}");
        assert!(!clean_defense(Some(
            "Committed and pushed the lab's work; nothing is left to do.\nATERM-TASK-DONE"
        )));
    }
}
