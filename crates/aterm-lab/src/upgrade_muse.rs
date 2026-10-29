// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `muse-upgrade [tag] [--keep]`: MUSE CODE'S LIVE UPGRADE, LIVE — its quiet flow (`[harness] upgrade_ask = false`, the default) on the real Muse Code under a private headless aterm with the harness ON, against the stub's Meta model API.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::door::Door;
use crate::pyjson::J;
use crate::relaunch_check::{host_said, newest_dump, request_with, said, wait_for};
use crate::relaunch_muse::{lock_pid, resumed, shell_exports, twin_script};
use crate::run::{Run, is_run};
use crate::stop;
use crate::util::now_ms;
use crate::{Fail, LAB, Res};

/// The word the first turn asks Muse Code to remember.
pub const WORD: &str = "KESTREL-5151";

/// How long phase 4's stub holds its answer: past the turn-end policy's two minutes of busy work (`TurnEndTiming::min_work`), so the turn is real work and the loop continues it at once.
pub const LONG_TURN_MS: i64 = 130_000;

/// The host's line on Muse Code's own activation notice (`harness_host`'s activation wake).
pub const NOTICED: &str = "an activation notice for Muse Code";

fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("muse-upgrade: COULD NOT RUN: {m}"))
}

/// The store build id of a revision-form version `x.y.z-R<n>[.<s>]` (atpkg's `vendor_direct::version`: `2·10^18 + x·10^15 + y·10^12 + z·10^9 + n·10^3 + s'`, `s'` 0 without `.<s>`, else `s + 1`).
#[must_use]
pub fn revision_build_id(version: &str) -> Option<u64> {
    let (plain, rev) = version.split_once("-R")?;
    let num = |s: &str| -> Option<u64> {
        (!s.is_empty()
            && s.bytes().all(|b| b.is_ascii_digit())
            && (s == "0" || !s.starts_with('0')))
        .then(|| s.parse().ok())
        .flatten()
    };
    let mut parts = plain.split('.');
    let (x, y, z) = (
        num(parts.next()?)?,
        num(parts.next()?)?,
        num(parts.next()?)?,
    );
    if parts.next().is_some() || x > 999 || y > 999 || z > 999 {
        return None;
    }
    let (n, sub) = match rev.split_once('.') {
        Some((n, s)) => (num(n)?, num(s)?.checked_add(1)?),
        None => (num(rev)?, 0),
    };
    if n > 999_999 || sub > 999 {
        return None;
    }
    Some(
        2_000_000_000_000_000_000
            + x * 1_000_000_000_000_000
            + y * 1_000_000_000_000
            + z * 1_000_000_000
            + n * 1_000
            + sub,
    )
}

/// `version` with its patch raised by `k` (`1.4.10-R6427.1` → `1.4.11-R6427.1` for 1): newer in atpkg's revision-aware order.
#[must_use]
pub fn bumped(version: &str, k: u64) -> Option<String> {
    let (plain, rev) = version.split_once("-R")?;
    let mut parts: Vec<&str> = plain.split('.').collect();
    let patch: u64 = parts.pop()?.parse().ok()?;
    let raised = (patch + k).to_string();
    parts.push(&raised);
    Some(format!("{}-R{rev}", parts.join(".")))
}

/// A build record's text with its `version = "…"` line naming `version`.
#[must_use]
pub fn record_naming(record: &str, version: &str) -> String {
    let mut out: String = record
        .lines()
        .filter(|l| {
            !l.trim_start().starts_with("version ") && !l.trim_start().starts_with("version=")
        })
        .map(|l| format!("{l}\n"))
        .collect();
    out.push_str(&format!("version = \"{version}\"\n"));
    out
}

/// The scratch store: its prefix, the source build's record, its builds.
struct Store {
    /// `<scratch home>/Library/Application Support/aterm/pkg`.
    prefix: PathBuf,
    /// The lab's build the store's clones are made of.
    source: PathBuf,
    /// The record text the source build's `.vendor` holds (or one written).
    record: String,
    /// The lab's run directory, where its holds on the scratch sit.
    run: PathBuf,
}

impl Store {
    fn store(&self) -> PathBuf {
        self.prefix.join("store/muse")
    }

    /// Build `id` showed it ran (its composer drawn, its session's lock
    /// naming a Muse started from it): its hold on the scratch goes.
    fn ran(&self, id: u64) {
        crate::lab::release_scratch(&self.run, &format!("muse-{id}"));
    }

    fn twin(&self) -> PathBuf {
        self.prefix.join("agents/muse")
    }

    /// The program of build `id`.
    fn bin(&self, id: u64) -> PathBuf {
        self.store().join(id.to_string()).join("bin/muse")
    }

    /// LAND build `version` as atpkg lands one (the module's order): its
    /// tree, its record and `.ready`, `current` swapped, the twin rewritten,
    /// and the activation notice published; the scratch held for it first.
    fn land(&self, version: &str) -> Result<PathBuf, String> {
        let id = revision_build_id(version).ok_or("no build id")?;
        let bin = self.bin(id);
        crate::lab::hold_scratch(&self.run, &format!("muse-{id}"))
            .map_err(|e| format!("its hold on the scratch: {e}"))?;
        crate::sys::put(&bin, 0o755, &crate::sys::PutSrc::File(&self.source))
            .map_err(|e| format!("the build's tree: {e}"))?;
        let store = self.store();
        crate::sys::put(
            &store.join(format!("{id}.vendor")),
            0o644,
            &crate::sys::PutSrc::Bytes(record_naming(&self.record, version).as_bytes()),
        )
        .map_err(|e| format!("its record: {e}"))?;
        crate::sys::put(
            &store.join(format!("{id}.ready")),
            0o644,
            &crate::sys::PutSrc::Bytes(b""),
        )
        .map_err(|e| format!("its .ready: {e}"))?;
        let tmp = store.join(format!(".current-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(id.to_string(), &tmp).map_err(|e| format!("current: {e}"))?;
        let current = store.join("current");
        std::fs::rename(&tmp, &current).map_err(|e| format!("current: {e}"))?;
        let resolved = std::fs::canonicalize(&current).map_err(|e| format!("current: {e}"))?;
        let build = bin
            .parent()
            .and_then(Path::parent)
            .and_then(|b| std::fs::canonicalize(b).ok());
        if build.as_deref() != Some(resolved.as_path()) {
            return Err(format!("current resolves to {}", resolved.display()));
        }
        crate::sys::put(
            &self.twin(),
            0o755,
            &crate::sys::PutSrc::Bytes(twin_script(&bin.to_string_lossy()).as_bytes()),
        )
        .map_err(|e| format!("the twin: {e}"))?;
        crate::sys::put(
            &self.prefix.join("activation-notices/muse"),
            0o600,
            &crate::sys::PutSrc::Bytes(b"1\n"),
        )
        .map_err(|e| format!("the activation notice: {e}"))?;
        Ok(bin)
    }
}

/// The Muse Code processes under the lab's instance run from the scratch
/// store (`<store>/<build>/bin/muse …`), `(pid, build, words)`.
fn store_muses(run: &Run, store: &Path) -> Vec<(i32, String, Vec<String>)> {
    let prefix = store.to_string_lossy().into_owned();
    crate::procs::descendants(run.instance_pid)
        .into_iter()
        .filter_map(|pid| {
            let cmd = crate::procs::command_line(pid);
            let rest = cmd.strip_prefix(&prefix)?.strip_prefix('/')?;
            let (build, after) = rest.split_once("/bin/muse")?;
            (after.is_empty() || after.starts_with(' ')).then(|| {
                (
                    pid,
                    build.to_string(),
                    after.split_whitespace().map(str::to_string).collect(),
                )
            })
        })
        .collect()
}

/// How many MAIN turns the stub has answered Muse Code (`kind=muse`, never
/// a `muse-side` call: the reminder agents and the prompt hint Muse asks on
/// its own), by the stub's log: a typed turn — a continuation — is one.
fn main_turns(run: &Run) -> usize {
    crate::util::scratch_text(&run.w.join("stub.log"))
        .lines()
        .filter(|l| l.contains(" kind=muse ") && l.contains("POST /responses"))
        .count()
}

/// The descendants of `pid` (its tool shells).
fn children(pid: i32) -> usize {
    crate::procs::descendants(pid).len()
}

/// The session whose `.session.lock` names `pid`, and that lock.
fn session_of(run: &Run, pid: i32) -> Option<(String, PathBuf)> {
    let root = run.w.join("home/.local/share/muse/sessions");
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect()
    };
    dirs(&root)
        .iter()
        .flat_map(|y| dirs(y))
        .flat_map(|m| dirs(&m))
        .flat_map(|d| dirs(&d))
        .find_map(|s| {
            let lock = s.join(".session.lock");
            let session = s.file_name()?.to_string_lossy().into_owned();
            (lock_pid(&crate::util::scratch_text(&lock)) == Some(i64::from(pid)))
                .then_some((session, lock))
        })
}

/// One upgrade ledger row: `(t, step, detail)`.
pub(crate) fn ledger(run: &Run, since_ms: i64) -> Vec<(i64, String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "ledger.jsonl")
                && p.to_string_lossy().contains("upgrade")
            {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&run.state(), &mut files);
    let mut rows: Vec<(i64, String, String)> = files
        .iter()
        .flat_map(|p| {
            crate::util::scratch_text(p)
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter_map(|l| {
            let r = J::parse(&l).ok()?;
            let t = r.get("t").and_then(J::as_i64)?;
            let s = |k: &str| r.get(k).and_then(J::as_str).unwrap_or("").to_string();
            (t * 1000 >= since_ms - 999).then(|| (t, s("step"), s("detail")))
        })
        .collect();
    rows.sort_by_key(|r| r.0);
    rows
}

/// The host's own upgrade steps that WAITED since `since_ms`, by its journal (`drive/*.journal.jsonl`: `HOST … upgrade step=wait:<why>`): `(ms, word)`.
pub(crate) fn held_steps(run: &Run, since_ms: i64) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, String)> = std::fs::read_dir(run.state().join("drive"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".journal.jsonl"))
        .flat_map(|p| {
            crate::util::scratch_text(&p)
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter_map(|l| {
            let r = J::parse(&l).ok()?;
            let t = r.get("t").and_then(J::as_i64)?;
            let line = r.get("line").and_then(J::as_str)?;
            let word = line
                .split_once(" upgrade step=wait:")?
                .1
                .split_whitespace()
                .next()?
                .to_string();
            (t >= since_ms).then(|| (t, format!("wait:{word}")))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The held steps said, each at its offset from `since_ms`.
pub(crate) fn show_held(run: &Run, since_ms: i64) {
    for (t, word) in held_steps(run, since_ms) {
        println!(
            "  held      +{:.1} s  {word}",
            f64::from(i32::try_from(t - since_ms).unwrap_or(i32::MAX)) / 1000.0
        );
    }
}

/// The row of `step` among `rows`, its time.
pub(crate) fn row_at(rows: &[(i64, String, String)], step: &str) -> Option<i64> {
    rows.iter().find(|(_, s, _)| s == step).map(|(t, _, _)| *t)
}

/// Whether the screen shows `needle`, by `ctl text`; `None` when that read
/// failed (the client did not run, a nonzero exit, an `ERR` line, nothing
/// drawn): a failed read is no reading, so a poll skips it and looks again.
pub(crate) fn shows(door: &Door<'_>, needle: &str) -> Option<bool> {
    let r = door.ctl(&["text"]).ok()?;
    if r.code != 0 || r.stdout.starts_with("ERR") || r.stdout.trim().is_empty() {
        return None;
    }
    Some(r.stdout.contains(needle))
}

/// Whether `text`, a reply, ends with the last tick of the stub's `slow`
/// reply: the stub sends that tick last, so a reply cut short ends before it.
pub(crate) fn ends_on_last_tick(text: &str) -> bool {
    text.trim_end()
        .ends_with(&format!("tick {}", crate::stub::SLOW_SECONDS))
}

/// Whether Muse's session record `session` (its `session.jsonl`) holds the
/// stub's `slow` turn ended whole: a run whose `assistant_message_committed`
/// text ends with the reply's last tick, and whose `terminal` record then
/// says `completed`. The run ends there, not at the commit: Muse's
/// end-of-turn gate runs about half a second between the two, and the
/// restart's G-M1 holds a run open until its `terminal`.
fn muse_reply_whole(session: &str) -> bool {
    let records: Vec<J> = session.lines().filter_map(|l| J::parse(l).ok()).collect();
    let runs: Vec<(&str, &J)> = records
        .iter()
        .filter_map(|r| {
            let p = r.get("payload")?;
            if p.get("kind").and_then(J::as_str) != Some("run") {
                return None;
            }
            Some((p.get("run_id").and_then(J::as_str)?, p.get("event")?))
        })
        .collect();
    fn kind(e: &J) -> Option<&str> {
        e.get("kind").and_then(J::as_str)
    }
    runs.iter().any(|(run, e)| {
        kind(e) == Some("assistant_message_committed")
            && e.get("text")
                .and_then(J::as_str)
                .is_some_and(ends_on_last_tick)
            && runs.iter().any(|(r, t)| {
                r == run
                    && kind(t) == Some("terminal")
                    && t.get("terminal").and_then(J::as_str) == Some("completed")
            })
    })
}

/// [`muse_reply_whole`] of the session file at `path`; `Err` names a file that could not be read.
fn slow_reply_whole(path: &Path) -> Result<bool, String> {
    std::fs::read_to_string(path)
        .map(|text| muse_reply_whole(&text))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// A reply-record reading as a leg prints it.
pub(crate) fn said_whole(r: &Result<bool, String>) -> String {
    match r {
        Ok(whole) => whole.to_string(),
        Err(why) => format!("unread ({why})"),
    }
}

/// How often [`held_while`] looks.
const HELD_POLL: Duration = Duration::from_millis(500);

/// How long after the work is seen ended the agent must still stand, with no
/// `terminated` row: past the time agy and Muse Code take to die on a SIGTERM
/// (gone within 0.1 s and 0.25 s: measured), so work that ended BECAUSE the
/// restart killed it is never read as work that ended first.
const STOOD_AFTER: Duration = Duration::from_millis(400);

/// A RESTART HELD WHILE THE WORK RAN, AND NOT TAKEN WITH ITS END: `(held while it ran, when it was seen ended)`, looking every [`HELD_POLL`] for at most `polls` looks, the agent `pid`, the ledger since `since`; `running` says whether the work runs, `None` when its read failed (that look sees nothing, never an end).
///
/// A restart taken within one look of the work's end reads as not held, so it
/// judges only work whose end wakes the agent, which the restart then waits
/// out (a task's or a subagent's end message to agy, a turn Muse opens when
/// its shell ends); a turn's end wakes nothing, and its legs read the agent's
/// record of the reply ([`ends_on_last_tick`]).
pub(crate) fn held_while(
    run: &Run,
    pid: i32,
    since: i64,
    polls: u32,
    running: impl FnMut() -> Option<bool>,
) -> Result<(bool, Option<i64>), Fail> {
    held_while_with(
        polls,
        |d| Ok(stop::sleep(d)?),
        || !crate::procs::alive(pid) || row_at(&ledger(run, since), "terminated").is_some(),
        running,
        now_ms,
    )
}

/// [`held_while`] over its clock and witnesses: `sleep` waits, `gone` says
/// whether the agent is ended, `running` whether the work runs (`None`: its
/// read failed, and that look sees nothing), `now` the time (ms).
fn held_while_with(
    polls: u32,
    mut sleep: impl FnMut(Duration) -> Result<(), Fail>,
    mut gone: impl FnMut() -> bool,
    mut running: impl FnMut() -> Option<bool>,
    mut now: impl FnMut() -> i64,
) -> Result<(bool, Option<i64>), Fail> {
    for _ in 0..polls {
        sleep(HELD_POLL)?;
        if gone() {
            return Ok((false, None));
        }
        if running() == Some(false) {
            let end = now();
            sleep(STOOD_AFTER)?;
            if gone() {
                return Ok((false, None));
            }
            return Ok((true, Some(end)));
        }
    }
    Ok((true, None))
}

/// The least time the lab's 40-second work (a `sleep 40` task, a subagent whose model call the stub holds 40 s) must be seen running from when it was first seen to its end: one that ended sooner did not run its course.
pub(crate) const RAN_MS: i64 = 35_000;

/// What one restart onto `build` looked like, read off the real thing.
struct Moved {
    pid: i32,
    words: Vec<String>,
    lock_names: bool,
    rows: Vec<(i64, String, String)>,
}

/// Wait up to `secs` for a Muse resuming `session` from build `build`, other
/// than `old`; then the lock and the ledger since `since`.
fn moved_onto(
    run: &Run,
    store: &Path,
    build: u64,
    session: &str,
    old: i32,
    secs: u64,
    since: i64,
) -> Result<Option<Moved>, Fail> {
    let found = wait_for(secs, || {
        store_muses(run, store).into_iter().find(|(pid, b, w)| {
            *pid != old && *b == build.to_string() && resumed(w) == Some(session)
        })
    })?;
    let Some((pid, _, words)) = found else {
        return Ok(None);
    };
    let _ = wait_for(60, || {
        ledger(run, since)
            .iter()
            .any(|(_, s, _)| s.starts_with("done:"))
            .then_some(())
    })?;
    let lock_names = session_of(run, pid).is_some_and(|(s, _)| s == session);
    Ok(Some(Moved {
        pid,
        words,
        lock_names,
        rows: ledger(run, since),
    }))
}

/// The rows said, one per line.
pub(crate) fn show_rows(rows: &[(i64, String, String)]) {
    for (t, s, d) in rows {
        println!("  ledger    {t} {s}  {d}");
    }
}

/// Whether `rows` say a quiet restart: `terminated`, `relaunched`, then `done:resumed`, and no `continued`.
pub(crate) fn quiet(rows: &[(i64, String, String)]) -> bool {
    let has = |step: &str| rows.iter().any(|(_, s, _)| s == step);
    has("terminated")
        && has("relaunched")
        && has("done:resumed")
        && !rows.iter().any(|(_, s, _)| s.starts_with("continued"))
}

/// One `muse-upgrade` run on the lab `tag`, which `cmd_muse_upgrade` brought up.
#[allow(clippy::too_many_lines)]
fn run_check(tag: &str) -> Result<bool, Fail> {
    let mut run = Run::load(tag)?;
    crate::muse::need_muse(&run).map_err(|f| could_not(&f.msg.unwrap_or_default()))?;
    let source = PathBuf::from(&run.muse_bin);
    println!(
        "muse-upgrade: lab '{tag}', Muse Code {} ({}), {}",
        run.muse_ver, run.muse_bin, run.aterm
    );
    let mut failed = Vec::new();
    let source_record = source
        .parent()
        .and_then(Path::parent)
        .map(|b| PathBuf::from(format!("{}.vendor", b.display())))
        .map(|p| crate::util::scratch_text(&p))
        .filter(|t| t.contains("version"))
        .unwrap_or_else(|| {
            format!(
                "schema = 1\nprogram = \"muse\"\nversion = \"{}\"\nvendor = \"Meta\"\n",
                run.muse_ver
            )
        });
    let store = Store {
        prefix: run.w.join("home/Library/Application Support/aterm/pkg"),
        source: source.clone(),
        record: source_record,
        run: run.run.clone(),
    };
    let v1 = run.muse_ver.clone();
    let (Some(v2), Some(v3), Some(v4), Some(v5)) = (
        bumped(&v1, 1),
        bumped(&v1, 2),
        bumped(&v1, 3),
        bumped(&v1, 4),
    ) else {
        return Err(could_not(&format!("{v1} is no revision-form version")));
    };
    let (Some(b1), Some(b2), Some(b3), Some(b4), Some(b5)) = (
        revision_build_id(&v1),
        revision_build_id(&v2),
        revision_build_id(&v3),
        revision_build_id(&v4),
        revision_build_id(&v5),
    ) else {
        return Err(could_not("a version with no build id"));
    };
    store
        .land(&v1)
        .map_err(|e| could_not(&format!("laying {v1}: {e}")))?;
    let rc = run.w.join("home/.zshrc");
    crate::sys::put(
        &rc,
        0o644,
        &crate::sys::PutSrc::Bytes(shell_exports(&run).as_bytes()),
    )
    .map_err(|e| could_not(&format!("the shell startup: {e}")))?;
    let door = Door::new(&run)?;
    let rc_s = rc.to_string_lossy().replace('\'', "'\\''");
    door.ctl(&["send", &format!(". '{rc_s}'")])?;
    door.ctl(&["key", "enter"])?;
    stop::sleep(Duration::from_millis(500))?;
    println!(
        "  store     {} — {v1} at {b1}, the twin {}",
        store.store().display(),
        store.twin().display()
    );

    let mut through = run.clone();
    through.muse_bin = store.twin().to_string_lossy().into_owned();
    crate::muse::launch(
        &through,
        &crate::muse::Launch {
            no_wait: true,
            args: vec!["--trust-workspace".to_string()],
            ..crate::muse::Launch::default()
        },
    )?;
    run = Run::load(tag)?;
    let door = Door::new(&run)?;
    if !door.seen("^❯", 60)? {
        return Err(could_not("Muse Code never drew its composer"));
    }
    store.ran(b1);
    crate::verbs::turn(&door, &format!("remember the word {WORD}"))?;
    if !door.seen(r"\(reply 1\)", 60)? {
        return Err(could_not("the stub's first answer never showed"));
    }
    let Some((pid1, session)) = wait_for(30, || {
        store_muses(&run, &store.store())
            .into_iter()
            .find_map(|(pid, b, _)| {
                (b == b1.to_string())
                    .then(|| session_of(&run, pid).map(|(s, _)| (pid, s)))
                    .flatten()
            })
    })?
    else {
        return Err(could_not(
            "no Muse Code from the scratch store holds a session lock",
        ));
    };
    println!("  launched  Muse pid {pid1} from {b1}, session {session}");
    door.await_idle(2000, 60)?;

    let turns_before = main_turns(&run);
    let t1 = now_ms();
    store
        .land(&v2)
        .map_err(|e| could_not(&format!("landing {v2}: {e}")))?;
    println!("  landed    {v2} at {b2} (the twin re-pointed, the activation notice published)");
    let noticed = wait_for(30, || host_said(&run, t1, &[NOTICED]))?;
    println!(
        "  noticed   {}",
        noticed
            .as_deref()
            .map_or("NO host line on the notice within 30 s", |l| l
                .split_once("harness:")
                .map_or(l, |(_, s)| s.trim()))
    );
    let moved1 = moved_onto(&run, &store.store(), b2, &session, pid1, 120, t1)?;
    let Some(m1) = moved1 else {
        show_rows(&ledger(&run, t1));
        said(
            false,
            "1. an idle Muse is restarted onto the new build within 120 s",
            &mut failed,
        );
        return Ok(false);
    };
    show_rows(&m1.rows);
    if m1.lock_names {
        store.ran(b2);
    }
    stop::sleep(Duration::from_secs(5))?;
    let silent = main_turns(&run) == turns_before;
    println!(
        "  restarted Muse pid {} from {b2}: {}; its session's lock names it={}; no turn reached the stub since the landing={silent}",
        m1.pid,
        m1.words.join(" "),
        m1.lock_names
    );
    door.await_idle(2000, 60)?;
    let before_turn = newest_dump(&run);
    crate::verbs::turn(&door, "what was the word?")?;
    let carried = wait_for(60, || request_with(&run, before_turn, &[WORD]))?;
    println!(
        "  next turn {}",
        carried.map_or_else(
            || "NO request carried the earlier word within 60 s".to_string(),
            |n| format!("req-{n:05} carried `{WORD}`: the prior turns came back")
        )
    );
    said(
        noticed.is_some()
            && m1.lock_names
            && quiet(&m1.rows)
            && silent
            && carried.is_some()
            && !crate::procs::alive(pid1),
        "1. an idle Muse is restarted onto the landed build on its session (argv, lock), nothing typed after; the stub sees the prior turns",
        &mut failed,
    );
    door.await_idle(2000, 60)?;

    crate::verbs::set_tool(
        &run,
        r#"{"name":"bash","input":{"command":"echo lab-bg-start; sleep 40; echo lab-bg-end","description":"lab background wait","sandbox_permissions":"require_escalated","yield_time_ms":1000}}"#,
    )?;
    crate::verbs::turn(&door, "toolstart")?;
    let child = wait_for(30, || {
        if door.out(&["text"]).contains("Yes, proceed") {
            let _ = door.ctl(&["key", "1"]);
        }
        (children(m1.pid) > 0).then_some(())
    })?;
    let backgrounded = wait_for(30, || door.out(&["text"]).contains("running").then_some(()))?;
    let t2 = now_ms();
    store
        .land(&v3)
        .map_err(|e| could_not(&format!("landing {v3}: {e}")))?;
    println!(
        "  bg shell  a child under pid {}={}, the panel running={}; landed {v3} at {b3}",
        m1.pid,
        child.is_some(),
        backgrounded.is_some()
    );
    let (held_while_running, ended_at) =
        held_while(&run, m1.pid, t2, 120, || Some(children(m1.pid) > 0))?;
    println!(
        "  bg ended  {}; the Muse stood while it ran={held_while_running}",
        ended_at.map_or_else(
            || "NOT within 60 s".to_string(),
            |t| format!("after {} ms", t - t2)
        )
    );
    let moved2 = moved_onto(&run, &store.store(), b3, &session, m1.pid, 180, t2)?;
    if moved2.as_ref().is_some_and(|m| m.lock_names) {
        store.ran(b3);
    }
    let after_end = match (&moved2, ended_at) {
        (Some(m), Some(end)) => {
            row_at(&m.rows, "terminated").is_some_and(|t| t * 1000 >= end - 999)
        }
        _ => false,
    };
    show_held(&run, t2);
    if let Some(m) = &moved2 {
        show_rows(&m.rows);
        println!(
            "  restarted Muse pid {} from {b3}: {}; its lock names it={}",
            m.pid,
            m.words.join(" "),
            m.lock_names
        );
    }
    said(
        child.is_some()
            && held_while_running
            && ended_at.is_some()
            && after_end
            && moved2
                .as_ref()
                .is_some_and(|m| m.lock_names && quiet(&m.rows)),
        "2. a Muse whose backgrounded shell runs is not restarted while it runs, and is once it ended",
        &mut failed,
    );
    let Some(m2) = moved2 else {
        return Ok(false);
    };
    door.await_idle(2000, 90)?;

    crate::verbs::set_mode(&run, "slow")?;
    crate::verbs::turn(&door, "a slow turn")?;
    let busy = door.seen("esc to interrupt", 30)?;
    let t3 = now_ms();
    store
        .land(&v4)
        .map_err(|e| could_not(&format!("landing {v4}: {e}")))?;
    // The turn's end wakes nothing, so the restart can follow it before the
    // next look at the screen, which then cannot say which came first; Muse's
    // record of the turn can: ended whole after the restart, not at the landing.
    let record = session_of(&run, m2.pid)
        .filter(|(s, _)| *s == session)
        .map(|(_, lock)| lock.with_file_name("session.jsonl"));
    let whole = |when: &str| match &record {
        Some(path) => slow_reply_whole(path),
        None => Err(format!("no session lock of pid {} {when}", m2.pid)),
    };
    let open = whole("at the landing");
    println!(
        "  busy      the turn running={busy}; landed {v4} at {b4}; the reply whole then={}",
        said_whole(&open)
    );
    let moved3 = moved_onto(&run, &store.store(), b4, &session, m2.pid, 180, t3)?;
    if moved3.as_ref().is_some_and(|m| m.lock_names) {
        store.ran(b4);
    }
    let ended = whole("after the restart");
    println!(
        "  reply     Muse's session record holds the slow turn ended whole={}",
        said_whole(&ended)
    );
    show_held(&run, t3);
    if let Some(m) = &moved3 {
        show_rows(&m.rows);
        println!(
            "  restarted Muse pid {} from {b4} after the turn: {}; its lock names it={}",
            m.pid,
            m.words.join(" "),
            m.lock_names
        );
    }
    said(
        busy && open == Ok(false)
            && ended == Ok(true)
            && moved3
                .as_ref()
                .is_some_and(|m| m.lock_names && quiet(&m.rows)),
        "3. a busy Muse is not restarted mid-turn, and is once its turn ended",
        &mut failed,
    );
    let Some(m3) = moved3 else {
        let _ = crate::verbs::set_mode(&run, "ok");
        return Ok(false);
    };

    crate::verbs::set_harness(&run, &[("continue".to_string(), "true".to_string())])?;
    stop::sleep(Duration::from_secs(5))?;
    door.await_idle(2000, 90)?;
    crate::verbs::set_custom_mode(
        &run,
        "lab-long-turn",
        &J::obj([
            ("delay_ms", J::Int(LONG_TURN_MS)),
            (
                "text",
                J::s("Part of the task is done; the next part is still ahead of me."),
            ),
        ]),
    )?;
    crate::verbs::turn(&door, "a long turn with the loop on")?;
    let busy4 = door.seen("esc to interrupt", 30)?;
    let t4 = now_ms();
    let turns_at_landing = main_turns(&run);
    store
        .land(&v5)
        .map_err(|e| could_not(&format!("landing {v5}: {e}")))?;
    println!("  driven    continue=true, the turn running={busy4}; landed {v5} at {b5}");
    let mut turns_at_restart = None;
    let mut turn_end4 = None;
    for _ in 0..720 {
        stop::sleep(Duration::from_millis(500))?;
        if row_at(&ledger(&run, t4), "terminated").is_some() || !crate::procs::alive(m3.pid) {
            turns_at_restart = Some(main_turns(&run));
            break;
        }
        if turn_end4.is_none() && shows(&door, "esc to interrupt") == Some(false) {
            turn_end4 = Some(now_ms());
        }
    }
    let moved4 = moved_onto(&run, &store.store(), b5, &session, m3.pid, 180, t4)?;
    if moved4.as_ref().is_some_and(|m| m.lock_names) {
        store.ran(b5);
    }
    show_held(&run, t4);
    let between = turns_at_restart.map(|n| n.saturating_sub(turns_at_landing));
    println!(
        "  driven    the turn ended {}; turns the stub answered from the landing to the restart: {}",
        turn_end4.map_or_else(|| "NOT seen".to_string(), |t| format!("+{} ms", t - t4)),
        between.map_or_else(|| "NO restart".to_string(), |n| n.to_string())
    );
    if let Some(m) = &moved4 {
        show_rows(&m.rows);
        println!(
            "  restarted Muse pid {} from {b5} at that turn's end: {}; its lock names it={}",
            m.pid,
            m.words.join(" "),
            m.lock_names
        );
    }
    let _ = crate::verbs::set_mode(&run, "ok");
    said(
        busy4
            && turn_end4.is_some()
            && between == Some(1)
            && moved4
                .as_ref()
                .is_some_and(|m| m.lock_names && quiet(&m.rows)),
        "4. with the loop's continuation on, the restart takes the turn end the loop would continue at: no turn typed between",
        &mut failed,
    );
    if failed.is_empty() {
        println!("muse-upgrade: PASS");
        Ok(true)
    } else {
        println!("muse-upgrade: FAIL: {}", failed.join("; "));
        Ok(false)
    }
}

/// `muse-upgrade [tag] [--keep]`.
pub fn cmd_muse_upgrade(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("muse-upgrade: {m}"));
    let mut tag = None;
    let mut keep = false;
    for a in args {
        match a.as_str() {
            "--keep" => keep = true,
            "-h" | "--help" => {
                print!("{}", crate::USAGE);
                return Ok(0);
            }
            s if s.starts_with('-') => return Err(usage(&format!("unknown option '{s}'"))),
            s => tag = Some(s.to_string()),
        }
    }
    let tag = tag.unwrap_or_else(|| format!("muse-upgrade-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: muse-upgrade brings up a lab of its own (another tag, or {LAB} down {tag})"
        )));
    }
    stop::install();
    let mut u = crate::lab::UpOpts::from_env();
    u.trust = true;
    u.lifeline = !keep;
    u.harness_keys = format!("{}\ncontinue = false", u.harness_keys);
    if let Err(f) = crate::lab::up(&tag, &u) {
        if let Some(m) = &f.msg {
            eprintln!("aterm-lab: {m}");
        }
        if f.stopped.is_some() {
            return Err(f);
        }
        eprintln!("aterm-lab: muse-upgrade: COULD NOT RUN: the lab did not come up");
        return Ok(3);
    }
    let verdict = run_check(&tag);
    if keep {
        println!("muse-upgrade: lab '{tag}' left up ({LAB} down {tag})");
    } else {
        let _ = crate::lab::down(&tag, false);
    }
    match verdict? {
        true => Ok(0),
        false => Ok(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_is_whole_once_muse_committed_its_last_tick_and_its_run_completed() {
        let record = |run: &str, event: &str| {
            format!(
                r#"{{"schema_version":1,"record_type":"event","payload_type":"runtime.session","payload":{{"kind":"run","run_id":"{run}","event":{event}}}}}"#
            )
        };
        let committed = |run: &str, text: &str| {
            record(
                run,
                &format!(r#"{{"kind":"assistant_message_committed","text":"{text}"}}"#),
            )
        };
        let ended = |run: &str, how: &str| {
            record(run, &format!(r#"{{"kind":"terminal","terminal":"{how}"}}"#))
        };
        let ticks = |to: i64| (1..=to).map(|i| format!("tick {i} ")).collect::<String>();
        let last = crate::stub::SLOW_SECONDS;
        let whole = ticks(last);
        let log = |rows: &[String]| rows.join("\n");
        assert!(muse_reply_whole(&log(&[
            committed("r1", &whole),
            ended("r1", "completed")
        ])));
        assert!(
            !muse_reply_whole(&committed("r1", &whole)),
            "a reply committed whole whose run has not ended: the end-of-turn gate still runs"
        );
        assert!(
            !muse_reply_whole(&log(&[
                committed("r1", &ticks(last - 1)),
                ended("r1", "completed")
            ])),
            "a reply cut before its last tick"
        );
        assert!(
            !muse_reply_whole(&log(&[committed("r1", &whole), ended("r2", "completed")])),
            "another run's end"
        );
        assert!(
            !muse_reply_whole(&log(&[committed("r1", &whole), ended("r1", "cancelled")])),
            "a run that did not complete"
        );
        let approval = r#"{"payload":{"kind":"approval_wait_effect","run_id":"r1","record":{"kind":"terminal"}}}"#;
        assert!(
            !muse_reply_whole(&log(&[committed("r1", &whole), approval.to_string()])),
            "another record's terminal"
        );
        assert!(
            !muse_reply_whole(&log(&[
                record("r1", &format!(r#"{{"kind":"status","text":"{whole}"}}"#)),
                ended("r1", "completed")
            ])),
            "another event's words"
        );
        assert!(
            !ends_on_last_tick(&format!("tick {last}0")),
            "a longer reply's tick"
        );
    }

    /// One scripted run of [`held_while_with`] over eight looks: the `gone`
    /// answers in order, one per ask (the confirming ask after the work is
    /// seen ended takes the next), and the `running` answers likewise (`None`
    /// a failed read); the clock one second an ask.
    fn judged(looks: &[(bool, Option<bool>)]) -> (bool, Option<i64>) {
        let gones: std::cell::RefCell<std::collections::VecDeque<bool>> =
            std::cell::RefCell::new(looks.iter().map(|l| l.0).collect());
        let runs: std::cell::RefCell<std::collections::VecDeque<Option<bool>>> =
            std::cell::RefCell::new(looks.iter().map(|l| l.1).collect());
        let clock = std::cell::Cell::new(0);
        held_while_with(
            8,
            |_| Ok(()),
            || gones.borrow_mut().pop_front().unwrap_or(true),
            || runs.borrow_mut().pop_front().unwrap_or(Some(false)),
            || {
                clock.set(clock.get() + 1000);
                clock.get()
            },
        )
        .expect("no stop")
    }

    /// THE JUDGE OF A RESTART HELD WHILE WORK RAN: work seen ended while the
    /// agent still stands, and still stands a moment later, is held and ended
    /// (`(true, Some)`).
    #[test]
    fn work_that_ends_with_the_agent_is_never_read_as_ended_first() {
        let (ran, ended) = (Some(true), Some(false));
        assert_eq!(
            judged(&[(false, ran), (false, ended), (false, ran)]),
            (true, Some(1000))
        );
        assert_eq!(judged(&[(false, ran), (true, ended)]), (false, None));
        assert_eq!(
            judged(&[(false, ran), (false, ended), (true, ran)]),
            (false, None)
        );
        assert_eq!(judged(&[(false, ran), (true, ran)]), (false, None));
        assert_eq!(judged(&[(false, ran); 8]), (true, None));
    }

    /// A FAILED READ IS NO END: a look whose read failed sees nothing, so an
    /// agent restarted on the next look was restarted while the work ran
    /// (read as an end, that look would have judged it held and ended); an
    /// end seen after it still counts, and reads that all fail see none.
    #[test]
    fn a_failed_read_is_never_read_as_the_work_ended() {
        let (ran, ended, failed) = (Some(true), Some(false), None);
        assert_eq!(
            judged(&[(false, ran), (false, failed), (false, ran), (true, ran)]),
            (false, None)
        );
        assert_eq!(
            judged(&[(false, ran), (false, failed), (false, ended), (false, ran)]),
            (true, Some(1000))
        );
        assert_eq!(judged(&[(false, failed); 8]), (true, None));
    }

    /// The build id atpkg lays a revision-form version under, as its store
    /// holds them (the scratch-agents copy's `2001004010006427002` is
    /// `1.4.10-R6427.1`).
    #[test]
    fn a_revision_versions_build_id_is_atpkgs() {
        assert_eq!(
            revision_build_id("1.4.10-R6427.1"),
            Some(2_001_004_010_006_427_002)
        );
        assert_eq!(
            revision_build_id("1.4.11-R6500.1"),
            Some(2_001_004_011_006_500_002)
        );
        assert_eq!(
            revision_build_id("1.4.10-R6427"),
            Some(2_001_004_010_006_427_000)
        );
        for bad in ["1.4.10", "1.04.10-R1", "1.4.1000-R1", "1.4.10-R1.999", "x"] {
            assert_eq!(revision_build_id(bad), None, "{bad}");
        }
    }

    /// A bumped version is the next patch, its revision kept; a record names
    /// it once, its other lines kept.
    #[test]
    fn a_landing_names_a_newer_version_in_its_record() {
        assert_eq!(
            bumped("1.4.10-R6427.1", 1).as_deref(),
            Some("1.4.11-R6427.1")
        );
        assert_eq!(
            bumped("1.4.10-R6427.1", 3).as_deref(),
            Some("1.4.13-R6427.1")
        );
        assert_eq!(bumped("1.4.10", 1), None);
        let r = record_naming(
            "schema = 1\nprogram = \"muse\"\nversion = \"1.4.10-R6427.1\"\nvendor = \"Meta\"\n",
            "1.4.11-R6427.1",
        );
        assert_eq!(r.matches("version = ").count(), 1, "{r}");
        assert!(r.contains("version = \"1.4.11-R6427.1\"") && r.contains("vendor = \"Meta\""));
    }

    /// A LANDED BUILD HOLDS THE SCRATCH UNTIL IT SHOWED IT RAN: each landing
    /// holds it before its clone is laid, and a build shown to run releases
    /// its own hold only. NEGATIVE CONTROL: the other build's hold stays.
    #[test]
    fn a_landed_build_holds_the_scratch_until_it_showed_it_ran() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let source = root.join("muse");
        std::fs::write(&source, "a build").unwrap();
        let store = Store {
            prefix: root.join("pkg"),
            source,
            record: String::new(),
            run: root.join("run"),
        };
        let versions = ["1.4.10-R6427.1", "1.4.11-R6427.1"];
        let ids = versions.map(|v| revision_build_id(v).unwrap());
        let held = |id: u64| {
            root.join("run")
                .join(crate::lab::HELD)
                .join(format!("muse-{id}"))
        };
        for v in versions {
            assert_eq!(
                store.land(v).unwrap(),
                store.bin(revision_build_id(v).unwrap()),
                "{v}"
            );
        }
        assert!(held(ids[0]).is_dir() && held(ids[1]).is_dir());
        store.ran(ids[0]);
        assert!(!held(ids[0]).exists(), "shown to run");
        assert!(held(ids[1]).is_dir(), "the other build still holds it");
    }
}
