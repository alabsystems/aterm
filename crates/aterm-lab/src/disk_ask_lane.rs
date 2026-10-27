// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `disk-ask [tag] [--keep] [--scenario clear|partial|unanswered|exhausted|worker]`: the ask to
//! free disk space (`disk-free@v1`, `docs/DESIGN-disk-free-ask-2026-10-24.md`) end to end, on a
//! real aterm with real Claude Code.
//!
//! A jailed lab of its own ([`crate::lab::up`]) whose `HOME` is a sparse APFS image
//! (`LAB_HOME_IMAGE`), its window the owner's disk watch there under the watch's development seam
//! ([`aterm_agent::harness::disk::WATCH_SEAM`]). The home holds one checkout per tab, each with a
//! recent build that aterm's own pass leaves (its bytes in `deps/`), and ballast that is no build,
//! so the volume stays below its floor once aterm's pass has done what it can. Claude Code tabs,
//! each in its own checkout and given a task, rest under the harness. The stub answers the
//! disk line, and only it, with a Bash call that removes the tab's `target/`, or, for a tab the
//! scenario makes silent, with words alone. What each tab received is read off every scratch
//! transcript, so a line in a conversation no tab's token names fails. Each scenario has its own
//! sizes ([`Geometry`]), held at compile time, and its own checks ([`Scenario`]):
//! - `clear` (the default): the largest build's tab is the one typed the line, which names that
//!   build as the tab's own; its cleanup lifts the volume past its floor and the margin, and the
//!   ledger and `aterm harness disk why` credit it. Then the lane fills the volume below its floor
//!   again and holds that the next looks open no episode, the volume's last line within the
//!   cooldown ([`ask::COOLDOWN_MS`]), that the look claims aterm's disk worker instead (the
//!   design's §8.4), which ends with no tab typed anything, and that no tab is typed a second
//!   line.
//! - `partial`: the asked tab frees more than [`ask::FREED_MIN`] but the volume stays below its
//!   floor, so the episode is settled in its name and, over three more looks of a short volume,
//!   nobody else is asked, no worker is claimed and nothing ends it.
//! - `unanswered`: the asked tab answers with words and frees nothing, so [`ask::ANSWER_MS`] after
//!   its line the ask is handed off; aterm's disk worker is claimed and the rounds wait on it, no
//!   tab typed anything, and once it is over the tab whose build is the next largest is asked; its
//!   cleanup settles the episode with the volume still short, and the third tab is never asked.
//!   The settle is what spares the third tab here, not the cap on lines ([`ask::MAX_ASKS`]).
//! - `exhausted`: the cap. As `unanswered`, but the second tab answers with words too, so its
//!   answer window passes with nothing freed and the episode's [`ask::MAX_ASKS`] lines are spent:
//!   the ledger runs it out (`out`, `nothing-freed`) instead of handing the ask on, no second worker
//!   is claimed ([`ask::WORKER_COOLDOWN_MS`]), and over three more looks of a short volume the third
//!   tab, whose build would be next, is never asked.
//! - `worker`: aterm's disk worker (the design's §8.4) end to end. Two tabs, each held by a draft
//!   left unsent in its composer, so no tab can be asked: the round waits on them and a look
//!   claims the worker, which starts in a new session in its own folder. No Claude Code ran a
//!   command in the lab's config folder before it, so no shell snapshot stands and its first box,
//!   its dry run, is declined with that reason and its remedy, `pwd`, not the fence's text; once
//!   its `pwd` ran it is fenced to free only through `aterm harness disk free` (its `rm -rf` of the
//!   cache the home holds is declined with the fence's reason, and the cache is still there after
//!   it), the verb refuses the ballast and a tab's build (approved, each path refused and
//!   journalled so), and frees the cache, through a link in it to the ballast (approved,
//!   journalled, the volume risen by its size and no more, the ballast and every build whole). It
//!   reports (`Nothing left:` and its `disk-free filling:` and `disk-free owner-only:` lines) and is
//!   over as `finished`, typed nothing after its report. No tab is elected, typed anything or loses
//!   its draft, and over three more looks of a volume still short no second worker, session or
//!   conversation of its task and no line come.
//!
//! The window starts the worker with plain `claude`, found by name on the session's `PATH`, as an
//! install's managed Claude Code is: every scenario that claims a worker brings its lab up with
//! the reroute ([`UpOpts::reroute`]), whose stub on that `PATH` runs a clone of the owner's Claude
//! Code, so the worker runs real Claude Code against the lab's stub, which drives it through its
//! steps by its task's tag ([`worker_script`]): `pwd` first, as its task asks, then its dry run.
//! The lane waits on its end, a hang detector of its longest life ([`ask::SPAWN_MS`] and
//! [`ask::WORKER_MS`]), and holds that it started, ran in its own folder, ran `pwd` with no box
//! and its dry run approved under its fence, reported, and finished. Wherever it runs, it runs in
//! a session of its own: a worker row naming a tab's session, or its task (`(rule disk-free@v1
//! worker)`) in a tab's conversation, fails the lane, as that is the owner's tab asked a second
//! time.
//!
//! What `worker` needs of the lab beyond that: its tabs keep their drafts under the owner's
//! grace ([`DRAFT_GRACE`]), and its instance's disk scans read the jail's processes alone
//! ([`UpOpts::jail_scan`]), as from inside the jail the owner's cannot be read and the verb rules
//! out no path while one stands unread. The shell snapshot the worker's fence proves its commands
//! against is the worker's own to write: no tab runs a command.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ask::Miss;
use aterm_agent::harness::disk::{EMERGENCY_FREE_GIB, EMERGENCY_FREE_PCT, ask};

use crate::LAB;
use crate::disk_lane::{Verdict, fill, free_on, lay_source, lay_target_sized};
use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::J;
use crate::run::{Run, is_run};
use crate::util::atomic_write;
use crate::workers::kv;
use crate::{Fail, Res, stop};

const GIB: u64 = 1 << 30;

/// The largest home image any scenario lays, in GiB: the host's disk is shared.
const MAX_IMAGE_GIB: u64 = 20;

/// What a home image holds past the builds, the cache and the free space the lane lays: the file
/// system's own, what the Claude Code sessions write in their homes, and, where a worker is
/// claimed, the clone of Claude Code the reroute lays there (about a quarter of a GiB). The rest
/// is ballast.
const HEADROOM: u64 = GIB;

/// How far from a line the lane's sizes keep what a tab's cleanup leaves: what Claude Code's own
/// writes and aterm's own pass move meanwhile never carry the volume across it.
const SPARE: u64 = GIB;

/// The sizes one scenario stands on.
#[derive(Debug)]
struct Geometry {
    /// The home image, in GiB.
    image_gib: u64,
    /// `[disk] auto_free_gib`: the floor, past the emergency line (4 GiB on the image), so the
    /// volume reads `short` between them.
    floor_gib: u64,
    /// `[disk] warn_free_gib`, above the floor.
    warn_gib: u64,
    /// Where free space sits once the home is laid: below the floor, past the emergency line.
    laid_free: u64,
    /// The tabs: each one's checkout and the build it holds, in GiB, the first the largest, so the
    /// election (`ask::rank`, the whole GiB a session's own checkout holds first) picks it.
    tabs: &'static [(&'static str, u64)],
    /// Which tabs' agents answer the disk line with words alone, removing nothing, one per tab.
    silent: &'static [bool],
    /// The tool cache the home holds for aterm's disk worker to free ([`CACHE`]), in GiB: `0`, no
    /// cache.
    cache_gib: u64,
}

impl Geometry {
    const fn floor(&self) -> u64 {
        self.floor_gib * GIB
    }

    const fn tab(&self, i: usize) -> u64 {
        self.tabs[i].1 * GIB
    }

    /// Every tab's build, in bytes.
    const fn builds(&self) -> u64 {
        let (mut n, mut i) = (0, 0);
        while i < self.tabs.len() {
            n += self.tab(i);
            i += 1;
        }
        n
    }

    const fn cache(&self) -> u64 {
        self.cache_gib * GIB
    }

    /// The home image's size as `hdiutil` spells it.
    fn image(&self) -> String {
        format!("{}g", self.image_gib)
    }

    /// The lane's `[disk]`.
    fn disk_toml(&self) -> String {
        format!(
            "[disk]\nwarn_free_gib = {}\nauto_free_gib = {}\nidle_build_dirs = true\n",
            self.warn_gib, self.floor_gib
        )
    }
}

/// What every scenario's sizes hold: two tabs or more, each told silent or not; the first tab's
/// build is the largest by whole GiB, so the election picks it; the floor and the warning sit
/// above the emergency line, which is 4 GiB on the image (its 1% is less); the home is laid below
/// its floor and past the emergency line; and the image holds the builds, the cache, the free
/// space laid and [`HEADROOM`], in at most [`MAX_IMAGE_GIB`], and a GiB less would not hold them,
/// so the image is no larger than the sizes need.
const fn sound(g: &Geometry) {
    assert!(g.tabs.len() >= 2 && g.silent.len() == g.tabs.len());
    let mut i = 1;
    while i < g.tabs.len() {
        assert!(g.tabs[0].1 > g.tabs[i].1);
        i += 1;
    }
    assert!(g.floor_gib > EMERGENCY_FREE_GIB && g.warn_gib > g.floor_gib);
    assert!(g.image_gib * GIB / 100 * EMERGENCY_FREE_PCT < EMERGENCY_FREE_GIB * GIB);
    assert!(g.laid_free < g.floor() && g.laid_free > EMERGENCY_FREE_GIB * GIB);
    let need = g.builds() + g.cache() + g.laid_free + HEADROOM;
    assert!(need <= g.image_gib * GIB && g.image_gib <= MAX_IMAGE_GIB);
    assert!((g.image_gib - 1) * GIB < need);
}

/// `clear`: the floor 5 GiB, laid 4.6 GiB free, and the first tab's build alone lifts the volume
/// past the floor plus [`ask::END_MARGIN`].
const CLEAR: Geometry = Geometry {
    image_gib: 16,
    floor_gib: 5,
    warn_gib: 6,
    laid_free: 46 * GIB / 10,
    tabs: &[("alpha", 7), ("beta", 2), ("gamma", 1)],
    silent: &[false; 3],
    cache_gib: 0,
};

/// `partial`: the floor raised to 8 GiB, so the volume laid just past the emergency line can rise
/// by the first tab's build, past [`ask::FREED_MIN`], and stay below it.
const PARTIAL: Geometry = Geometry {
    image_gib: 10,
    floor_gib: 8,
    warn_gib: 9,
    laid_free: 46 * GIB / 10,
    tabs: &[("alpha", 2), ("beta", 1), ("gamma", 1)],
    silent: &[false; 3],
    cache_gib: 0,
};

/// `unanswered`: the first tab answers with words alone; the second, whose build is larger than
/// the third's by whole GiB, so the round after the hand-off elects it, frees past
/// [`ask::FREED_MIN`] with the volume still below its floor, so the episode is settled.
const UNANSWERED: Geometry = Geometry {
    image_gib: 12,
    floor_gib: 8,
    warn_gib: 9,
    laid_free: 46 * GIB / 10,
    tabs: &[("alpha", 3), ("beta", 2), ("gamma", 1)],
    silent: &[true, false, false],
    cache_gib: 0,
};

/// `exhausted`: `unanswered`'s sizes with its second tab silent too, so the episode spends its
/// [`ask::MAX_ASKS`] lines on the two largest builds, freeing nothing; the third tab's is the build
/// a line past the cap would name.
const EXHAUSTED: Geometry = Geometry {
    silent: &[true, true, false],
    ..UNANSWERED
};

/// `worker`: two tabs, each held by a draft, so no line can go and the round waits on them, and
/// a tool cache ([`CACHE`]) of 2 GiB for aterm's disk worker to free; freeing it leaves the
/// volume below its floor, as the ballast is no cache.
const WORKER: Geometry = Geometry {
    image_gib: 11,
    floor_gib: 8,
    warn_gib: 9,
    laid_free: 46 * GIB / 10,
    tabs: &[("alpha", 2), ("beta", 1)],
    silent: &[false, false],
    cache_gib: 2,
};

/// How many of `g`'s tabs are silent.
const fn silent(g: &Geometry) -> usize {
    let (mut n, mut i) = (0, 0);
    while i < g.silent.len() {
        if g.silent[i] {
            n += 1;
        }
        i += 1;
    }
    n
}

// The geometry the scenarios stand on, each beside what it is for.
const _: () = {
    sound(&CLEAR);
    // Removing the first tab's build alone ends the episode, past the floor and its margin, with
    // a GiB to spare.
    assert!(CLEAR.laid_free + CLEAR.tab(0) >= CLEAR.floor() + ask::END_MARGIN + SPARE);
    assert!(!CLEAR.silent[0]);
    sound(&PARTIAL);
    // The first tab's build is past what settles the episode, and removing it leaves the volume
    // below its floor, each with room to spare.
    assert!(PARTIAL.tab(0) >= ask::FREED_MIN + SPARE);
    assert!(PARTIAL.laid_free + PARTIAL.tab(0) + SPARE <= PARTIAL.floor());
    assert!(!PARTIAL.silent[0]);
    sound(&UNANSWERED);
    // The first tab frees nothing; the round after it elects the second, whose build is past what
    // settles the episode and leaves the volume below its floor; the third is never asked.
    assert!(UNANSWERED.silent[0] && !UNANSWERED.silent[1]);
    assert!(UNANSWERED.tabs[1].1 > UNANSWERED.tabs[2].1);
    assert!(UNANSWERED.tab(1) >= ask::FREED_MIN + SPARE);
    assert!(UNANSWERED.laid_free + UNANSWERED.tab(1) + SPARE <= UNANSWERED.floor());
    sound(&EXHAUSTED);
    // The cap's lines go to silent tabs, the largest builds first (the second's larger than the
    // third's by whole GiB, so the round after the hand-off elects it), and one tab is left that
    // a line past the cap would go to.
    assert!(silent(&EXHAUSTED) == ask::MAX_ASKS as usize);
    assert!(EXHAUSTED.silent[0] && EXHAUSTED.silent[1] && !EXHAUSTED.silent[2]);
    assert!(EXHAUSTED.tabs[1].1 > EXHAUSTED.tabs[2].1);
    sound(&WORKER);
    // The cache lifts the volume by more than what reads as freed and the sessions' own writes,
    // and removing it leaves the volume below its floor with a GiB to spare, so the round still
    // waits on the drafted tabs once the worker is over, and the looks after it prove that no
    // second worker and no line come.
    assert!(WORKER.cache() >= ask::FREED_MIN + SPARE);
    assert!(WORKER.laid_free + WORKER.cache() + SPARE <= WORKER.floor());
    // Only the worker scenario lays a cache: every other one's worker finds none.
    assert!(CLEAR.cache_gib + PARTIAL.cache_gib + UNANSWERED.cache_gib + EXHAUSTED.cache_gib == 0);
};

/// Which run of the ask the lane makes, each its own [`Geometry`] and checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// The asked tab's cleanup clears the volume past its floor and the margin: the episode ends,
    /// and the volume filled short again within the cooldown opens none.
    Clear,
    /// The asked tab frees past [`ask::FREED_MIN`] with the volume still short: the episode is
    /// settled in its name, and nobody else is asked while the volume stays short.
    Partial,
    /// The asked tab answers with words alone: [`ask::ANSWER_MS`] after its line the ask is handed
    /// off to one other tab, whose cleanup settles the episode, and the third is never asked.
    Unanswered,
    /// Both tabs asked answer with words alone: the second's miss spends [`ask::MAX_ASKS`], the
    /// episode runs out, and the third is never asked.
    Exhausted,
    /// Every tab holds a draft, so none can be asked: aterm's disk worker frees the cache through
    /// `aterm harness disk free` alone, reports and finishes, and no tab is asked or typed.
    Worker,
}

impl Scenario {
    const ALL: [Scenario; 5] = [
        Scenario::Clear,
        Scenario::Partial,
        Scenario::Unanswered,
        Scenario::Exhausted,
        Scenario::Worker,
    ];

    const fn word(self) -> &'static str {
        match self {
            Scenario::Clear => "clear",
            Scenario::Partial => "partial",
            Scenario::Unanswered => "unanswered",
            Scenario::Exhausted => "exhausted",
            Scenario::Worker => "worker",
        }
    }

    fn named(word: &str) -> Option<Scenario> {
        Scenario::ALL.into_iter().find(|s| s.word() == word)
    }

    const fn geometry(self) -> &'static Geometry {
        match self {
            Scenario::Clear => &CLEAR,
            Scenario::Partial => &PARTIAL,
            Scenario::Unanswered => &UNANSWERED,
            Scenario::Exhausted => &EXHAUSTED,
            Scenario::Worker => &WORKER,
        }
    }

    /// Whether a look claims aterm's disk worker in this scenario, which then starts Claude Code
    /// by name, so its lab lays the reroute ([`UpOpts::reroute`]) as an install's managed Claude
    /// Code is laid.
    const fn claims_worker(self) -> bool {
        !matches!(self, Scenario::Partial)
    }

    /// Whether each tab is left a draft in its composer before the home is laid short, so the
    /// round can ask none of them.
    const fn drafted(self) -> bool {
        matches!(self, Scenario::Worker)
    }
}

/// What the host keeps free past the image before the lane lays anything, as `up` asks
/// ([`crate::lab::HOME_IMAGE_SPARE_GIB`]): the image is sparse, so the lane takes the host's space
/// only as it fills it, at most the image's size.
const HOST_SPARE: u64 = crate::lab::HOME_IMAGE_SPARE_GIB * GIB;

/// The custom stub mode the tabs talk to.
const MODE: &str = "diskask";

/// What each tab's agent says to anything but the disk line.
const WORKING: &str = "Working on it: this checkout's build comes next.";

/// What an asked tab's agent says once its tool call ran.
const REMOVED: &str = "Removed this checkout's build directory (target/) to free space.";

/// What a silent tab's agent says to the disk line, removing nothing.
const DECLINED: &str = "Noted: I am keeping this checkout's build for now and carrying on.";

/// The command an asked tab's agent runs (in its own checkout).
const CLEANUP: &str = "rm -rf target";

/// What aterm's disk worker runs first, as its task asks ([`ask::worker_task`]): a read Claude
/// Code runs with no box, after which its shell snapshot (`shell-snapshots/` in its config folder)
/// stands for the worker's fence to prove its other commands against. Claude Code writes one only
/// once its Bash tool ran a command, and removes it when its session ends.
const FIRST_BASH: &str = "pwd";

/// The tool cache the `worker` scenario lays under the home ([`Geometry::cache_gib`]): one that
/// `aterm harness disk free` admits (under `~/Library/Caches`, not kept by its name) and aterm's
/// own pass never reads.
const CACHE: &str = "Library/Caches/labtool";

/// The link the `worker` scenario lays in its cache ([`CACHE`]) to the ballast, which removing the
/// cache must not follow.
const ESCAPE: &str = "escape";

/// The ballast the lane fills the home with, which is no cache.
const BALLAST: &str = "ballast";

/// What aterm's disk worker runs to see what may go ([`worker_script`]).
const FREE_DRY: &str = "aterm harness disk free --dry-run";

/// The line `aterm harness disk free --dry-run` ends with, which only a run of it prints.
const WOULD_FREE: &str = "would free ";

/// The heads of the lines a disk worker's report ends with ([`ask::worker_task`]), after the
/// `Nothing left:` that starts it.
const REPORT_HEADS: [&str; 2] = ["disk-free filling:", "disk-free owner-only:"];

/// What starts a disk worker's report, as Autopilot Land reads a defense.
const NOTHING_LEFT: &str = aterm_agent::supervise::policy::needs::NOTHING_LEFT;

/// `[harness] human_grace_s` in a drafted scenario's lab ([`Scenario::drafted`]): the supervisor
/// keeps its hands off a tab for an hour after a person's keystroke, the knob's least power, as an
/// owner who wants a draft left alone sets it. Under the default two minutes a tab's turn-end
/// loop, once its grace is over, sends a draft standing at its point as the person's own words in
/// place of its continuation (`TurnEndAction::Submit`), so a draft holds a Cruise tab for two
/// minutes at most, too short for the round and the worker the scenario watches.
const DRAFT_GRACE: u32 = 3600;

/// The disk watch's first look after the window starts, and the period of its looks (the
/// window's `DISK_FIRST_LOOK` and `DISK_TICK`).
const FIRST_LOOK: Duration = Duration::from_secs(120);
const TICK: Duration =
    Duration::from_millis(aterm_agent::supervise::occupancy::reading::LOOK_EVERY_MS.unsigned_abs());

/// Slack on every hang detector: a look's pass, a turn break's wait, a busy host.
const SLACK: Duration = Duration::from_secs(90);

/// How long `clear` watches the volume short again, past the look that read it so and opened no
/// episode, for a second line: two more looks and half of one.
const QUIET: Duration = Duration::from_secs(5 * 30);

/// How long `partial` and `unanswered` watch the settled episode, the volume short, for a second
/// line or an end: three looks and half of one.
const SETTLED_QUIET: Duration = Duration::from_secs(7 * 30);

/// The hang detector on an asked tab's cleanup showing in the volume's free space.
const RISE: Duration = Duration::from_secs(60);

/// The hang detector on a tab's own step: its turn's end, its composer drawing a draft.
const HANG: Duration = Duration::from_secs(60);

/// One tab of the lane.
#[derive(Debug, Clone)]
struct Tab {
    name: &'static str,
    gib: u64,
    token: String,
    dir: PathBuf,
    sid: String,
    /// Its agent answers the disk line with words alone ([`Geometry::silent`]).
    silent: bool,
    /// What the lane leaves unsent in its composer ([`Scenario::drafted`]), which holds it.
    draft: Option<String>,
}

/// What a scenario with a cache ([`Geometry::cache_gib`]) lays for aterm's disk worker, and drives
/// it to give `aterm harness disk free` ([`worker_script`]).
#[derive(Debug, Clone)]
struct Planted {
    /// The home it is laid under, which the worker writes as `~` ([`free_in_home`]).
    home: PathBuf,
    /// The tool cache ([`CACHE`]), which the verb may remove.
    cache: PathBuf,
    /// The bytes laid in it.
    bytes: u64,
    /// What the verb must refuse: the ballast ([`BALLAST`]), which is no cache, and the last
    /// tab's build.
    outside: Vec<PathBuf>,
}

/// One `free-ask` row of the ledger, as the lane reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AskRow {
    ts: String,
    said: String,
    sid: String,
    step: String,
    why: String,
    text: String,
    free: Option<u64>,
    /// A `worker-over` row's: how far free space rose while the worker ran.
    rose: Option<u64>,
}

/// The `free-ask` rows of the ledger `text` (`disk.jsonl`), oldest first.
fn ask_rows(text: &str) -> Vec<AskRow> {
    text.lines()
        .filter_map(|l| J::parse(l).ok())
        .filter(|r| r.get("kind").and_then(J::as_str) == Some(ask::ROW_KIND))
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(J::as_str).unwrap_or("").to_owned();
            let n = |k: &str| {
                r.get(k)
                    .and_then(J::as_i64)
                    .and_then(|f| u64::try_from(f).ok())
            };
            AskRow {
                ts: s("ts"),
                said: s("said"),
                sid: s("sid"),
                step: s("step"),
                why: s("why"),
                text: s("text"),
                free: n("free_bytes"),
                rose: n("rose_bytes"),
            }
        })
        .collect()
}

/// One `disk-free` row of the ledger (`aterm harness disk free`'s journal), as the lane reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FreeRow {
    sid: String,
    /// `removing`, `removed`, `failed` or `refused`.
    action: String,
    path: String,
    bytes: Option<u64>,
}

/// The `disk-free` rows of the ledger `text` (`disk.jsonl`), oldest first.
fn free_rows(text: &str) -> Vec<FreeRow> {
    text.lines()
        .filter_map(|l| J::parse(l).ok())
        .filter(|r| {
            r.get("kind").and_then(J::as_str) == Some(aterm_agent::harness::disk::free::KIND)
        })
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(J::as_str).unwrap_or("").to_owned();
            FreeRow {
                sid: s("sid"),
                action: s("action"),
                path: s("path"),
                bytes: r
                    .get("bytes")
                    .and_then(J::as_i64)
                    .and_then(|b| u64::try_from(b).ok()),
            }
        })
        .collect()
}

/// One row of a session's approval ledger (`<state>/drive/<sid>.jsonl`), as the lane reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Decided {
    /// Unix ms.
    ts: i64,
    rule: String,
    /// `approved`, `declined`, `typed`, … ([`aterm_agent::supervise::approvals::Outcome`]).
    decision: String,
    command: String,
    reason: String,
}

/// The rows of an approval ledger's `text`, oldest first.
fn decisions(text: &str) -> Vec<Decided> {
    text.lines()
        .filter_map(|l| J::parse(l).ok())
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(J::as_str).unwrap_or("").to_owned();
            Decided {
                ts: r.get("ts").and_then(J::as_i64).unwrap_or(0),
                rule: s("rule_id"),
                decision: s("decision"),
                command: s("command"),
                reason: s("reason"),
            }
        })
        .collect()
}

/// Session `sid`'s approval ledger in `run`.
fn decided(run: &Run, sid: &str) -> Vec<Decided> {
    decisions(&crate::util::scratch_text(
        &aterm_agent::supervise::approvals::ledger_under(&run.state(), Some(sid)),
    ))
}

/// How many of `rows` said `said` (with `step`, for a worker's step row).
fn count(rows: &[AskRow], said: &str, step: &str) -> usize {
    rows.iter()
        .filter(|r| r.said == said && r.step == step)
        .count()
}

/// The ledger the lab's window writes its disk rows to; the ask's lease lies beside it.
fn ledger(run: &Run) -> PathBuf {
    run.state()
        .join("harness")
        .join(aterm_agent::harness::cli::HARNESS)
        .join("disk.jsonl")
}

/// What the scratch transcripts hold of the disk ask ([`received_in`]).
#[derive(Debug, Default, PartialEq, Eq)]
struct Received {
    /// Each tab's name and the lines its conversations received: `None` when no transcript's
    /// first prompt holds its token, so what it received cannot be read.
    tabs: Vec<(String, Option<Vec<String>>)>,
    /// A line in a transcript whose first prompt holds no tab's token, with that file.
    stray: Vec<(PathBuf, String)>,
    /// aterm's disk worker's task ([`worker_tag`]) in a tab's conversation (a transcript whose
    /// first prompt holds its token), with the tab's name and that file: the worker runs in a
    /// session of its own, so this is the owner's tab asked again. One in a conversation of no
    /// tab's (the worker's own) is not here.
    worker_in_tab: Vec<(String, PathBuf)>,
}

impl Received {
    /// Every line, each tab's and every stray one.
    fn total(&self) -> usize {
        self.tabs
            .iter()
            .filter_map(|(_, l)| l.as_ref())
            .map(Vec::len)
            .sum::<usize>()
            + self.stray.len()
    }

    /// The tabs no transcript was found for.
    fn unread(&self) -> Vec<&str> {
        self.tabs
            .iter()
            .filter(|(_, l)| l.is_none())
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// The lines each tab received, in the tabs' order (an unread tab's: none).
    fn counts(&self) -> Vec<usize> {
        self.tabs
            .iter()
            .map(|(_, l)| l.as_ref().map_or(0, Vec::len))
            .collect()
    }

    /// Whether every tab was read and received exactly `want` lines, in the tabs' order, no line
    /// lies in a conversation no tab's token names, and no tab's conversation holds the disk
    /// worker's task.
    fn exactly(&self, want: &[usize]) -> bool {
        self.unread().is_empty()
            && self.counts() == want
            && self.stray.is_empty()
            && self.worker_in_tab.is_empty()
    }

    /// The first line any conversation received.
    fn first(&self) -> Option<&str> {
        self.tabs
            .iter()
            .filter_map(|(_, l)| l.as_ref()?.first())
            .chain(self.stray.iter().map(|(_, l)| l))
            .map(String::as_str)
            .next()
    }

    /// The first line tab `i` received.
    fn first_of(&self, i: usize) -> Option<&str> {
        self.tabs.get(i)?.1.as_ref()?.first().map(String::as_str)
    }
}

/// The tag the disk line ends with, `(rule disk-free@v1)`: the disk worker's task ends with
/// `(rule disk-free@v1 worker)`, which is no line to a tab.
fn line_tag() -> String {
    format!("(rule {})", ask::RULE_DISK_FREE)
}

/// The tag the disk worker's task ends with, `(rule disk-free@v1 worker)` ([`ask::worker_task`]).
fn worker_tag() -> String {
    format!("(rule {} worker)", ask::RULE_DISK_FREE)
}

/// The disk-ask lines `all` (every transcript, with its file) holds: each user row that is no
/// tool's result and holds the line's tag ([`line_tag`]), counted for the tab whose token the
/// transcript's first prompt holds (every such transcript of the tab), or as stray, with its
/// file, when that prompt holds no tab's token; and each such row that holds the disk worker's
/// tag ([`worker_tag`]) in a tab's transcript.
fn received_in(all: &[(PathBuf, Vec<crate::workers::Row>)], tabs: &[Tab]) -> Received {
    let mut out = Received {
        tabs: tabs.iter().map(|t| (t.name.to_owned(), None)).collect(),
        stray: Vec::new(),
        worker_in_tab: Vec::new(),
    };
    for (file, rows) in all {
        let holding = |tag: String| {
            rows.iter()
                .filter(move |r| r.kind == "user" && r.result.is_none() && r.text.contains(&tag))
                .map(|r| r.text.clone())
        };
        let tab = crate::workers::first_prompt(rows)
            .and_then(|p| tabs.iter().position(|t| p.text.contains(&t.token)));
        match tab {
            Some(i) => {
                out.tabs[i]
                    .1
                    .get_or_insert_with(Vec::new)
                    .extend(holding(line_tag()));
                out.worker_in_tab
                    .extend(holding(worker_tag()).map(|_| (tabs[i].name.to_owned(), file.clone())));
            }
            None => out
                .stray
                .extend(holding(line_tag()).map(|l| (file.clone(), l))),
        }
    }
    out
}

/// What `want` (the lines each tab is to receive, in the tabs' order) says, one tab after another.
fn wanted(tabs: &[Tab], want: &[usize]) -> String {
    tabs.iter()
        .zip(want)
        .map(|(t, n)| match n {
            0 => format!("{} never", t.name),
            1 => format!("{} once", t.name),
            n => format!("{} {n} times", t.name),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `line` names tab `name`'s checkout's builds marked the asked tab's own, alone in its
/// checkout: `~/work/<name>'s builds <size> (this tab's)`.
fn marks_own(line: &str, name: &str) -> bool {
    line.split_once(&format!("~/work/{name}'s builds "))
        .and_then(|(_, rest)| rest.split_once(')'))
        .is_some_and(|(said, _)| said.ends_with(" (this tab's") && !said.contains(';'))
}

/// aterm's disk worker's conversations in `all` (every transcript, with its file): those whose
/// first prompt holds its task's tag ([`worker_tag`]).
fn worker_talks(
    all: &[(PathBuf, Vec<crate::workers::Row>)],
) -> Vec<&(PathBuf, Vec<crate::workers::Row>)> {
    all.iter()
        .filter(|(_, rows)| {
            crate::workers::first_prompt(rows).is_some_and(|p| p.text.contains(&worker_tag()))
        })
        .collect()
}

/// A conversation's report and what was typed after it: its last reply with words and no tool
/// call, and every row past it that typed it something (a user row with words that is no tool's
/// result).
fn report_of(
    rows: &[crate::workers::Row],
) -> (Option<&crate::workers::Row>, Vec<&crate::workers::Row>) {
    let at = rows
        .iter()
        .rposition(|r| r.kind == "assistant" && r.tool_use.is_none() && !r.text.trim().is_empty());
    let after = at.map_or_else(Vec::new, |i| {
        rows[i + 1..]
            .iter()
            .filter(|r| r.kind == "user" && r.result.is_none() && !r.text.trim().is_empty())
            .collect()
    });
    (at.map(|i| &rows[i]), after)
}

/// Whether `text` is a disk worker's report as its task asks for one: it starts `Nothing left:`
/// and holds a line under each of [`REPORT_HEADS`].
fn is_report(text: &str) -> bool {
    text.trim_start().starts_with(NOTHING_LEFT)
        && REPORT_HEADS
            .iter()
            .all(|head| text.lines().any(|l| l.trim_start().starts_with(head)))
}

/// The results, in order, of each Bash call in `rows` whose command is `command`.
fn results_of<'r>(rows: &'r [crate::workers::Row], command: &str) -> Vec<&'r str> {
    rows.iter()
        .filter(|r| {
            r.tool_use.as_ref().is_some_and(|(name, input)| {
                name == "Bash" && input.get("command").and_then(J::as_str) == Some(command)
            })
        })
        .filter_map(|call| {
            rows.iter()
                .find(|r| r.result.is_some() && r.tool_id == call.tool_id)
                .and_then(|r| r.result.as_deref())
        })
        .collect()
}

/// The `rm -rf` of `cache` aterm's disk worker is driven to try, which its fence declines.
fn rm_cache(cache: &Path) -> String {
    format!("rm -rf {}", cache.display())
}

/// The `aterm harness disk free` of `paths` aterm's disk worker is driven to run.
fn free_of(paths: &[&Path]) -> String {
    let named: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    format!("aterm harness disk free {}", named.join(" "))
}

/// The `aterm harness disk free` of `planted`'s outside paths written under `~`, short enough for
/// the one row its fence proves ([`aterm_agent::supervise::policy::DISK_WORKER_ONE_ROW`]).
fn free_in_home(planted: &Planted) -> String {
    let named: Vec<String> = planted
        .outside
        .iter()
        .map(|p| {
            p.strip_prefix(&planted.home).map_or_else(
                |_| p.display().to_string(),
                |rel| format!("~/{}", rel.display()),
            )
        })
        .collect();
    format!("aterm harness disk free {}", named.join(" "))
}

/// The report aterm's disk worker ends with: what it freed (the planted cache, or nothing), what
/// fills the volume and what only the owner can free. It names no dry run: Land reads one in a
/// defense, `--dry-run` too, as work left open.
fn worker_report(planted: Option<&Planted>) -> String {
    let freed = match planted {
        Some(p) => format!(
            "freed {} ({}) through `aterm harness disk free`; the `rm -rf` of it was declined, \
             so it went through the verb alone, and the verb refused ~/ballast and the build I \
             gave it, which are no cache.",
            p.cache.display(),
            aterm_agent::harness::disk::human_bytes(p.bytes)
        ),
        None => "freed nothing: `aterm harness disk free` listed no cache that may go.".to_owned(),
    };
    format!(
        "{NOTHING_LEFT} {freed}\n{} ~/ballast, which is no cache, and the builds under ~/work, which their tabs keep.\n{} ~/ballast.",
        REPORT_HEADS[0], REPORT_HEADS[1]
    )
}

/// The steps the stub drives aterm's disk worker through, keyed to its task's tag
/// ([`worker_tag`], the stub's keyed `script`), so no tab's conversation takes one. With nothing
/// `planted`: [`FIRST_BASH`], as its task asks, the dry run ([`FREE_DRY`]) and its report. With
/// a cache planted, as a worker that skipped its task's first step: the dry run before any
/// command ran (its fence declines it, no shell snapshot standing), [`FIRST_BASH`], a Bash `rm
/// -rf` of the cache (its fence declines it), the dry run again, `aterm harness disk free` of the
/// paths it must refuse ([`Planted::outside`]) by their whole paths, which Claude Code draws on
/// two rows (its fence declines it), then written under `~` on one row ([`free_in_home`]), the
/// cache's removal through it, then its report.
/// Autopilot Land answers a task's first reply with its own prompt and lands the task on a clean
/// `Nothing left:` answer to it, so the report is said again, as its defense.
fn worker_script(planted: Option<&Planted>) -> J {
    let bash = |command: String, description: &str| {
        J::obj([
            ("when", J::s(worker_tag())),
            ("name", J::s("Bash")),
            (
                "input",
                J::obj([
                    ("command", J::s(command)),
                    ("description", J::s(description)),
                ]),
            ),
        ])
    };
    let first = || bash(FIRST_BASH.to_owned(), "Print the working directory");
    let dry = || bash(FREE_DRY.to_owned(), "List the caches aterm may free");
    let mut steps = Vec::new();
    match planted {
        Some(p) => {
            let outside: Vec<&Path> = p.outside.iter().map(PathBuf::as_path).collect();
            steps.extend([
                dry(),
                first(),
                bash(rm_cache(&p.cache), "Remove the labtool cache"),
                dry(),
                bash(
                    free_of(&outside),
                    "Free the ballast and a build through aterm",
                ),
                bash(free_in_home(p), "Free them again on one row"),
                bash(free_of(&[&p.cache]), "Free the labtool cache through aterm"),
            ]);
        }
        None => steps.extend([first(), dry()]),
    }
    let report = J::obj([
        ("when", J::s(worker_tag())),
        ("text", J::s(worker_report(planted))),
    ]);
    steps.extend([report.clone(), report]);
    J::Arr(steps)
}

/// The stub's mode: [`WORKING`] to anything; to a message holding the disk line, in a silent
/// tab's conversation (its token in a prompt) [`DECLINED`] alone, and in any other a Bash call of
/// [`CLEANUP`], then [`REMOVED`] once its result is back; and aterm's disk worker's conversation
/// driven through [`worker_script`] of `planted`. A tab runs no command but its cleanup.
fn stub_mode(tabs: &[Tab], planted: Option<&Planted>) -> J {
    let call = J::obj([
        ("name", J::s("Bash")),
        (
            "input",
            J::obj([
                ("command", J::s(CLEANUP)),
                (
                    "description",
                    J::s("Remove this checkout's build directory"),
                ),
            ]),
        ),
    ]);
    let mut replies: Vec<J> = tabs
        .iter()
        .filter(|t| t.silent)
        .map(|t| {
            J::obj([
                ("when", J::s(t.token.as_str())),
                ("last", J::s(line_tag())),
                ("text", J::s(DECLINED)),
            ])
        })
        .collect();
    replies.push(J::obj([
        ("last", J::s(line_tag())),
        ("tool", call),
        ("text", J::s(REMOVED)),
    ]));
    J::obj([
        ("stream", J::Bool(true)),
        ("text", J::s(WORKING)),
        ("replies", J::Arr(replies)),
        ("script", worker_script(planted)),
    ])
}

/// A lane that could not run: what stopped it (exit 3).
fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("disk-ask: COULD NOT RUN: {m}"))
}

/// Wait, in slices an interrupt ends, until `ready` holds or `within` passes; whether it held.
/// A hang detector for something that must happen, never a latency budget.
fn wait_for(within: Duration, mut ready: impl FnMut() -> bool) -> Result<bool, Fail> {
    let end = Instant::now() + within;
    loop {
        if ready() {
            return Ok(true);
        }
        if Instant::now() >= end {
            return Ok(false);
        }
        stop::sleep(Duration::from_millis(500))?;
    }
}

/// `@sid await match <re>` for up to `within`: whether a row of that tab matched.
fn tab_shows(door: &Door<'_>, sid: &str, re: &str, within: Duration) -> Result<bool, Fail> {
    let at = format!("@{sid}");
    let end = Instant::now() + within;
    loop {
        stop::check()?;
        let left = end.saturating_duration_since(Instant::now());
        let ms = left
            .min(Duration::from_secs(2))
            .as_millis()
            .max(1)
            .to_string();
        let r = door.ctl(&[
            &at,
            "await",
            "match",
            &crate::door::wire_pattern(re),
            &format!("timeout={ms}"),
        ])?;
        let line = r.stdout.lines().next().unwrap_or("");
        if line == "OK" || (line.starts_with("OK ") && !line.starts_with("OK timeout")) {
            return Ok(true);
        }
        if !line.starts_with("OK timeout") || left.is_zero() {
            return Ok(false);
        }
    }
}

/// The lab a scenario runs on, its tabs up: what every step reads.
struct Lane<'a> {
    run: &'a Run,
    door: &'a Door<'a>,
    home: PathBuf,
    tabs: Vec<Tab>,
    ledger: PathBuf,
    floor: u64,
    /// [`Geometry::laid_free`].
    laid_free: u64,
    /// What was laid for aterm's disk worker, where a cache is.
    planted: Option<Planted>,
    t0: Instant,
    /// The ledger's rows printed so far.
    printed: Cell<usize>,
}

impl Lane<'_> {
    fn rows(&self) -> Vec<AskRow> {
        ask_rows(&crate::util::scratch_text(&self.ledger))
    }

    /// The ledger's `disk-free` rows.
    fn free_rows(&self) -> Vec<FreeRow> {
        free_rows(&crate::util::scratch_text(&self.ledger))
    }

    /// The folder aterm starts its disk worker in, beside the ledger ([`ask::WORKER_FOLDER`]).
    fn worker_folder(&self) -> PathBuf {
        self.ledger
            .parent()
            .map_or_else(PathBuf::new, |h| h.join(ask::WORKER_FOLDER))
    }

    /// Print the ledger's rows not yet printed.
    fn show(&self) {
        let all = self.rows();
        for r in all.iter().skip(self.printed.get()) {
            println!(
                "  row       {} {}{} {}{}{}",
                r.ts,
                r.said,
                if r.sid.is_empty() {
                    String::new()
                } else {
                    format!(" @{}", r.sid)
                },
                if r.step.is_empty() {
                    String::new()
                } else {
                    format!("{} ", r.step)
                },
                if r.why.is_empty() {
                    String::new()
                } else {
                    format!("why={} ", r.why)
                },
                r.free
                    .map_or_else(String::new, |f| format!("free={} MiB", f >> 20))
            );
        }
        self.printed.set(all.len());
    }

    /// Wait (a hang detector) until the ledger holds a row past the first `skip` that `want`
    /// takes, printing rows as they come; that row.
    fn wait_row(
        &self,
        within: Duration,
        skip: usize,
        want: impl Fn(&AskRow) -> bool,
    ) -> Result<Option<AskRow>, Fail> {
        let mut found = None;
        wait_for(within, || {
            self.show();
            found = self.rows().into_iter().skip(skip).find(|r| want(r));
            found.is_some()
        })?;
        Ok(found)
    }

    fn free(&self) -> u64 {
        free_on(&self.home).unwrap_or(0)
    }

    fn received(&self) -> Received {
        received_in(&crate::workers::transcripts(self.run), &self.tabs)
    }

    /// The ask's lease, as the window last wrote it.
    fn lease(&self) -> ask::Lease {
        self.ledger
            .parent()
            .map_or_else(ask::Lease::default, ask::read)
    }

    fn since(&self, at: Instant) -> String {
        format!("+{:.0}s", at.duration_since(self.t0).as_secs_f64())
    }

    /// Watch the volume for `within`, printing rows as they come: the most free space read.
    fn watch(&self, within: Duration) -> Result<u64, Fail> {
        let mut top = self.free();
        wait_for(within, || {
            self.show();
            top = top.max(self.free());
            false
        })?;
        Ok(top)
    }
}

/// Print what each tab received.
fn say(got: &Received) {
    for (name, lines) in &got.tabs {
        match lines {
            Some(l) => println!("  received  {name}: {} line(s)", l.len()),
            None => println!("  received  {name}: no transcript of it"),
        }
    }
    for (file, _) in &got.stray {
        println!("  received  a line in {}, no tab's", file.display());
    }
    for (name, file) in &got.worker_in_tab {
        println!(
            "  received  aterm's disk worker's task in {name}'s conversation ({})",
            file.display()
        );
    }
}

/// Check `line`, the line `tab` received: the rule's tag, `tab`'s own build named and marked so,
/// which the line can say only when the asked session's working directory read its checkout,
/// only this tab asked, within its cap.
fn line_is_the_rules(v: &mut Verdict, line: &str, tab: &Tab) {
    let chars = line.chars().count();
    println!("  line      ({chars} chars) {}", line.replace('\n', " "));
    v.check(
        &format!(
            "{}'s line is the rule's: its tag, {}'s build marked this tab's, only this tab asked, within its cap",
            tab.name, tab.name
        ),
        line.contains(&line_tag())
            && marks_own(line, tab.name)
            && line.contains("Only this tab was asked.")
            && chars <= ask::LINE_CAP,
        || format!("{chars} chars (cap {}): {line}", ask::LINE_CAP),
    );
}

/// What the head of every scenario found: the episode open, the largest build's tab elected and
/// typed its line.
struct Head {
    t_open: Instant,
    t_elected: Instant,
    t_line: Instant,
    /// Free space as the election was read, before the line.
    free_before: u64,
}

/// The head of every scenario: an episode opens once aterm's own pass is spent, the round elects
/// the first tab, whose checkout holds the most, and the line reaches its conversation, the rule's
/// own, naming that build as the tab's. `None`: a step the rest stands on did not come.
fn head(lane: &Lane<'_>, v: &mut Verdict) -> Result<Option<Head>, Fail> {
    let within = FIRST_LOOK + TICK * 3 + SLACK;
    let opened = lane.wait_row(within, 0, |r| r.said == "opened")?.is_some();
    v.check(
        "an episode opened once aterm's own pass was spent",
        opened,
        || {
            format!(
                "no `opened` row within {} s of the window's start; rows: {:?}",
                within.as_secs(),
                lane.rows()
            )
        },
    );
    if !opened {
        return Ok(None);
    }
    let t_open = Instant::now();
    println!("  opened    {}", lane.since(t_open));
    let nominate = Duration::from_millis(ask::NOMINATE_MS.unsigned_abs());
    let pick = lane
        .wait_row(nominate + TICK * 2 + SLACK, 0, |r| r.said == "elected")?
        .map(|r| r.sid);
    let biggest = &lane.tabs[0];
    v.check(
        &format!(
            "the round elected {} (@{}), whose checkout holds the most",
            biggest.name, biggest.sid
        ),
        pick.as_deref() == Some(biggest.sid.as_str()),
        || format!("pick={pick:?}; rows: {:?}", lane.rows()),
    );
    let Some(pick) = pick else {
        return Ok(None);
    };
    let t_elected = Instant::now();
    println!("  elected   @{pick} {}", lane.since(t_elected));
    let free_before = lane.free();
    let typed = wait_for(TICK * 2 + SLACK, || {
        lane.show();
        lane.received().total() > 0
    })?;
    let t_line = Instant::now();
    let got = lane.received();
    say(&got);
    v.check("the disk line reached a tab's conversation", typed, || {
        format!(
            "none within {} s of the election",
            (TICK * 2 + SLACK).as_secs()
        )
    });
    if !typed {
        return Ok(None);
    }
    line_is_the_rules(v, got.first().unwrap_or_default(), biggest);
    Ok(Some(Head {
        t_open,
        t_elected,
        t_line,
        free_before,
    }))
}

/// Tab `tab`'s cleanup, after its line (`free_before` free then): wait for its build's bytes to
/// come back and its `target/` to go, and check the rise and that its `target/` is gone; with
/// `short`, also that the volume stays below its floor. The rise's moment, and free space then.
fn rise(
    lane: &Lane<'_>,
    v: &mut Verdict,
    tab: &Tab,
    free_before: u64,
    short: bool,
) -> Result<(Instant, u64), Fail> {
    let build = tab.gib * GIB;
    let target = tab.dir.join("target");
    let risen = |free: u64| free >= free_before + build * 9 / 10;
    // A hang detector of the tool call's whole `rm`: the volume reads the rise as the build's
    // files go, a moment before `rm` takes target/ itself, so both are waited for, then read.
    wait_for(RISE + SLACK, || {
        lane.show();
        risen(lane.free()) && !target.exists()
    })?;
    let t_rise = Instant::now();
    let after = lane.free();
    let rose = risen(after);
    let gone = !target.exists();
    println!(
        "  freed     {} MiB -> {} MiB free (floor {} MiB) {}",
        free_before >> 20,
        after >> 20,
        lane.floor >> 20,
        lane.since(t_rise)
    );
    let why = || {
        format!(
            "{} MiB -> {} MiB, floor {} MiB, target/ there: {}",
            free_before >> 20,
            after >> 20,
            lane.floor >> 20,
            !gone
        )
    };
    if short {
        v.check(
            &format!(
                "the volume rose by {}'s {} GiB after its tool call (its target/ gone), past {} MiB yet below its floor",
                tab.name,
                tab.gib,
                ask::FREED_MIN >> 20
            ),
            rose && gone && after >= free_before + ask::FREED_MIN && after < lane.floor,
            why,
        );
    } else {
        v.check(
            &format!(
                "the volume rose by {}'s {} GiB after its tool call (its target/ gone)",
                tab.name, tab.gib
            ),
            rose && gone,
            why,
        );
    }
    Ok((t_rise, after))
}

/// Wait for the ledger to credit `tab` (a `freed` row in its name past the first `skip` rows), and
/// check it; whether it came.
fn credit(lane: &Lane<'_>, v: &mut Verdict, tab: &Tab, skip: usize) -> Result<bool, Fail> {
    let credited = lane
        .wait_row(TICK * 2 + SLACK, skip, |r| {
            r.said == "freed" && r.sid == tab.sid
        })?
        .is_some();
    v.check(
        &format!(
            "the ledger credits {} (@{}) with the space (a `freed` row)",
            tab.name, tab.sid
        ),
        credited,
        || format!("rows: {:?}", lane.rows()),
    );
    Ok(credited)
}

/// The ledger rows a scenario expects over its whole run, each word's count.
struct Counts {
    opened: usize,
    elected: usize,
    typed: usize,
    freed: usize,
    handoff: usize,
    /// `worker`: claims of aterm's disk worker.
    worker: usize,
    out: usize,
}

/// The sessions aterm's disk worker ran as, as the ledger's worker rows past the first `skip`
/// name them (`worker-started`, `worker-runs`, `worker-over`), each once, in order.
fn worker_sids(rows: &[AskRow], skip: usize) -> Vec<String> {
    let mut sids: Vec<String> = Vec::new();
    for r in rows.iter().skip(skip) {
        let worker = matches!(
            r.said.as_str(),
            "worker-started" | "worker-runs" | "worker-over"
        );
        if worker && !r.sid.is_empty() && !sids.contains(&r.sid) {
            sids.push(r.sid.clone());
        }
    }
    sids
}

/// Of `sids`, those that are a tab's session, each with that tab's name.
fn tabs_among<'t>(sids: &[String], tabs: &'t [Tab]) -> Vec<(&'t str, &'t str)> {
    tabs.iter()
        .filter(|t| sids.contains(&t.sid))
        .map(|t| (t.name, t.sid.as_str()))
        .collect()
}

/// What [`worker`] read of aterm's disk worker, once it was over.
struct Worked {
    /// Where the rounds read on from: the rows past its `worker-over`.
    resume: usize,
    /// The session it ran as, as its `worker-started` row names it.
    sid: Option<String>,
    /// Free space as its claim was read.
    free_at_claim: u64,
    /// Its `worker-over` row.
    over: AskRow,
}

/// What aterm's disk worker `sid` ended with, read off its conversation and its approval
/// ledger: its conversations' count (those whose first prompt is its task), its report (its last
/// reply's words and stamp, unix ms), and what was typed into it after that report (each row of
/// its conversation, and each `typed` row of its ledger, past it).
struct Reported {
    talks: usize,
    report: Option<(String, Option<i64>)>,
    typed_after: Vec<String>,
}

/// [`Reported`] of aterm's disk worker `sid`, read now.
fn reported(lane: &Lane<'_>, sid: &str) -> Reported {
    let all = crate::workers::transcripts(lane.run);
    let talks = worker_talks(&all);
    let (report, after) = talks
        .first()
        .map_or((None, Vec::new()), |(_, rows)| report_of(rows));
    let report = report.map(|r| (r.text.clone(), r.ts));
    let typed = aterm_agent::supervise::approvals::Outcome::Typed.word();
    let since = report.as_ref().and_then(|(_, ts)| *ts);
    let mut typed_after: Vec<String> = after
        .iter()
        .map(|r| format!("its conversation: {}", r.text))
        .collect();
    typed_after.extend(
        decided(lane.run, sid)
            .into_iter()
            .filter(|d| d.decision == typed && since.is_none_or(|s| d.ts > s))
            .map(|d| format!("its ledger: {} typed {}", d.rule, d.command)),
    );
    Reported {
        talks: talks.len(),
        report,
        typed_after,
    }
}

/// Check that aterm's disk worker `sid` ran Claude Code on its task and ended with its report
/// ([`is_report`]), and that nothing was typed into it after that report.
fn reports(lane: &Lane<'_>, v: &mut Verdict, sid: &str) {
    let r = reported(lane, sid);
    if let Some((text, _)) = &r.report {
        for l in text.lines() {
            println!("  report    | {l}");
        }
    }
    v.check(
        "aterm's disk worker ran Claude Code on its task (one conversation whose first prompt is its task) and ended with its report: a last reply starting `Nothing left:` with its `disk-free filling:` and `disk-free owner-only:` lines",
        r.talks == 1 && r.report.as_ref().is_some_and(|(t, _)| is_report(t)),
        || format!("{} conversation(s) of its task; its last reply: {:?}", r.talks, r.report),
    );
    v.check(
        "nothing was typed into aterm's disk worker after its report (no row of its conversation, no `typed` row of its approval ledger)",
        r.report.is_some() && r.typed_after.is_empty(),
        || format!("report: {:?}; typed after it: {:?}", r.report, r.typed_after),
    );
}

/// aterm's disk worker's one conversation (its task's) in `all`, or none.
fn worker_talk(all: &[(PathBuf, Vec<crate::workers::Row>)]) -> &[crate::workers::Row] {
    worker_talks(all)
        .first()
        .map_or(&[], |(_, rows)| rows.as_slice())
}

/// Whether approval-ledger row `d` is of a box of the Bash command `command`, by the subject its
/// rule wrote: the command alone, or after the box's kind (`bash box: <command>`), each read with
/// its whitespace dropped, as the box may wrap a long command over its rows.
fn boxed(d: &Decided, command: &str) -> bool {
    let (row, want) = (squashed(&d.command), squashed(command));
    row == want || row.ends_with(&format!("box:{want}"))
}

/// Check that aterm's disk worker `sid` ran [`FIRST_BASH`] with no box (no row of its approval
/// ledger), in its own folder (its result), and its dry run ([`FREE_DRY`]) under its fence:
/// approved under [`RULE_DISK_WORKER`], a result of it the verb's own summary ([`WOULD_FREE`]),
/// which only a run of it prints. A dry run its fence declined, for want of a shell snapshot or
/// any other reason, fails here, as that worker can free nothing.
fn ran_its_reads(lane: &Lane<'_>, v: &mut Verdict, sid: &str) {
    let all = crate::workers::transcripts(lane.run);
    let talk = worker_talk(&all);
    let ledger = decided(lane.run, sid);
    let folder = std::fs::canonicalize(lane.worker_folder()).ok();
    let pwd = results_of(talk, FIRST_BASH);
    for r in &pwd {
        println!("  {FIRST_BASH:<9} {}", r.trim());
    }
    let pwd_boxed: Vec<&Decided> = ledger.iter().filter(|d| boxed(d, FIRST_BASH)).collect();
    v.check(
        &format!(
            "aterm's disk worker ran `{FIRST_BASH}`, as its task asks, with no box (no row of its approval ledger), in its own folder"
        ),
        !pwd.is_empty()
            && pwd
                .iter()
                .all(|r| std::fs::canonicalize(r.trim()).ok() == folder)
            && folder.is_some()
            && pwd_boxed.is_empty(),
        || format!("its results {pwd:?} (its folder {folder:?}); its ledger's rows of it {pwd_boxed:?}"),
    );
    let rule = aterm_agent::supervise::policy::RULE_DISK_WORKER;
    let approved = aterm_agent::supervise::approvals::Outcome::Approved.word();
    let dry: Vec<&Decided> = ledger.iter().filter(|d| boxed(d, FREE_DRY)).collect();
    for d in &dry {
        println!("  dry run   {} {} {}", d.rule, d.decision, d.reason);
    }
    let ran = results_of(talk, FREE_DRY);
    let summary = ran
        .iter()
        .flat_map(|r| r.lines())
        .find(|l| l.trim_start().starts_with(WOULD_FREE));
    println!("  dry run   said {summary:?}");
    v.check(
        &format!(
            "aterm's disk worker's dry run (`{FREE_DRY}`) ran: approved under {rule}, its result the verb's `{}` line",
            WOULD_FREE.trim_end()
        ),
        dry.iter()
            .any(|d| d.rule == rule && d.decision == approved)
            && summary.is_some(),
        || format!("its ledger's rows of it {dry:?}; its results {ran:?}"),
    );
}

/// aterm's disk worker, which the window starts where no tab can free the volume in time
/// (`ask::worker_due`, the design's §8.4): wait for a look past the ledger's first `skip` rows to
/// claim it (a `worker` row; `due` says why it is due), for it to start (`worker-started`) and to
/// be over (`worker-over`), and check that it started in a new session, none of the tabs', in its
/// own folder ([`Lane::worker_folder`]); that it ran Claude Code on its task, reported, and was
/// typed nothing after ([`reports`]); that it ran `pwd` with no box and its dry run approved under
/// its fence ([`ran_its_reads`]); that it is over as `finished`; that while it was claimed or
/// running no round elected anyone and no tab was typed a line (each tab still at `want` lines,
/// read off every transcript); and that it ran in a session of its own: no worker row and no lease
/// names a tab's session as the worker's, and no tab's conversation holds its task. `None` when it
/// was never claimed or never over.
#[allow(clippy::too_many_lines)]
fn worker(
    lane: &Lane<'_>,
    v: &mut Verdict,
    skip: usize,
    due: &str,
    want: &[usize],
) -> Result<Option<Worked>, Fail> {
    let claimed = lane
        .wait_row(TICK * 2 + SLACK, skip, |r| r.said == "worker")?
        .is_some();
    let free_at_claim = lane.free();
    v.check(
        &format!("{due}, so a look claimed aterm's disk worker (a `worker` row)"),
        claimed,
        || format!("rows: {:?}", lane.rows()),
    );
    if !claimed {
        return Ok(None);
    }
    let t_claim = Instant::now();
    println!(
        "  worker    claimed {} ({} MiB free)",
        lane.since(t_claim),
        free_at_claim >> 20
    );
    let place = |rows: &[AskRow], from: usize, said: &str| {
        rows.iter()
            .enumerate()
            .skip(from)
            .find(|(_, r)| r.said == said)
            .map(|(i, _)| i)
    };
    let at = place(&lane.rows(), skip, "worker").unwrap_or(skip);
    // A hang detector of its spawn: a look reads it started, or over without starting.
    let spawn = Duration::from_millis(ask::SPAWN_MS.unsigned_abs());
    let started = lane
        .wait_row(spawn + TICK * 2 + SLACK, at, |r| {
            r.said == "worker-started" || r.said == "worker-over"
        })?
        .filter(|r| r.said == "worker-started" && !r.sid.is_empty())
        .map(|r| r.sid);
    let folder = lane.worker_folder();
    match &started {
        Some(sid) => {
            let at_sid = format!("@{sid}");
            let status = lane.door.out(&[&at_sid, "status"]);
            let cwd = lane.door.out(&[&at_sid, "cwd"]);
            let cwd = cwd.trim();
            let cwd = cwd.strip_prefix("OK ").unwrap_or(cwd).trim();
            println!(
                "  worker    started as @{sid} {}: supervisor={} autopilot={} agent={} cwd={cwd}",
                lane.since(Instant::now()),
                kv(&status, "supervisor").unwrap_or("-"),
                kv(&status, "autopilot").unwrap_or("-"),
                kv(&status, "agent").unwrap_or("-"),
            );
            let tab = lane.tabs.iter().find(|t| t.sid == *sid).map(|t| t.name);
            let here = std::fs::canonicalize(cwd).ok();
            let own = std::fs::canonicalize(&folder).ok();
            v.check(
                "aterm's disk worker started in a new session (a `worker-started` row), none of the tabs', in its own folder",
                tab.is_none() && here.is_some() && here == own,
                || {
                    format!(
                        "@{sid} is {}; its directory {cwd:?}, the worker's folder {}",
                        tab.map_or_else(|| "no tab".to_owned(), |t| format!("{t}'s session")),
                        folder.display()
                    )
                },
            );
        }
        None => v.check(
            "aterm's disk worker started in a new session (a `worker-started` row), none of the tabs', in its own folder",
            false,
            || {
                format!(
                    "no `worker-started` row within {} s of the claim: rows {:?}",
                    (spawn + TICK * 2 + SLACK).as_secs(),
                    lane.rows()
                )
            },
        ),
    }
    // A hang detector of the claim's longest life: its spawn, then its run.
    let life = Duration::from_millis((ask::SPAWN_MS + ask::WORKER_MS).unsigned_abs());
    let over = lane.wait_row(life + TICK * 2 + SLACK, at, |r| r.said == "worker-over")?;
    let got = lane.received();
    let rows = lane.rows();
    let end = place(&rows, at, "worker-over").unwrap_or(rows.len());
    if let Some(o) = &over {
        println!(
            "  worker    {}over why={} rose={} MiB {}",
            if o.sid.is_empty() {
                String::new()
            } else {
                format!("@{} ", o.sid)
            },
            o.why,
            o.rose.unwrap_or(0) >> 20,
            lane.since(Instant::now())
        );
    }
    if let Some(sid) = &started {
        reports(lane, v, sid);
        ran_its_reads(lane, v, sid);
    }
    v.check(
        "aterm's disk worker is over as `finished`: its task ended, not expired, stalled, gone or lost",
        over
            .as_ref()
            .is_some_and(|o| o.why == "finished" && started.as_deref() == Some(o.sid.as_str())),
        || format!("started as {started:?}; over: {over:?}"),
    );
    let mut sids = worker_sids(&rows, at);
    for e in &lane.lease().volumes {
        if let Some(s) = e.worker.as_ref().and_then(ask::Worker::sid)
            && !sids.iter().any(|x| x == s)
        {
            sids.push(s.to_owned());
        }
    }
    let tabs = tabs_among(&sids, &lane.tabs);
    let runs = count(&rows[at.min(end)..end], "worker-runs", "");
    println!(
        "  worker    ran as {} ({runs} `worker-runs` row(s) while it ran)",
        if sids.is_empty() {
            "no session (never started)".to_owned()
        } else {
            sids.iter()
                .map(|s| format!("@{s}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    v.check(
        "aterm's disk worker ran in a session of its own: no worker row and no lease names a tab's session as the worker's, and no tab's conversation holds its task",
        tabs.is_empty() && got.worker_in_tab.is_empty(),
        || {
            format!(
                "worker sessions {sids:?}, of them the tabs' {tabs:?}; its task in {:?}",
                got.worker_in_tab
            )
        },
    );
    let between = &rows[at.min(end)..end];
    v.check(
        &format!(
            "aterm's disk worker is over (a `worker-over` row), and while it was claimed or running no round elected anyone and the line reached {}",
            wanted(&lane.tabs, want)
        ),
        over.is_some()
            && count(between, "elected", "") == 0
            && count(between, "step", "typed") == 0
            && got.exactly(want),
        || format!("over: {over:?}; {got:?}; rows since the claim: {between:?}"),
    );
    Ok(over.map(|over| Worked {
        resume: end + 1,
        sid: started,
        free_at_claim,
        over,
    }))
}

/// How an episode parked, which no look but one that reads the volume clear moves for
/// [`ask::COOLDOWN_MS`].
#[derive(Debug, Clone, Copy)]
enum Parked<'a> {
    /// Settled in this tab's name (its `freed` row): the next look says `settled`.
    Settled(&'a Tab),
    /// Out, its lines spent (an `out` row): the next look says `spent`.
    Out,
}

/// After the episode parked (`parked`): watch [`SETTLED_QUIET`] of looks and check that the volume
/// stayed below its floor at every reading (or the checks after it would prove nothing), that the
/// next look read the episode parked so and none ended it, and that the ledger moved no further:
/// `opened`, `elected`, `typed`, `freed`, `handoff`, `worker` and `out` as `want` counts them.
fn parked_quiet(
    lane: &Lane<'_>,
    v: &mut Verdict,
    parked: Parked<'_>,
    want: &Counts,
) -> Result<(), Fail> {
    let top = lane.watch(SETTLED_QUIET)?;
    v.check(
        &format!(
            "the volume stayed below its floor through {} s of looks after the episode parked (at most {} MiB free, floor {} MiB)",
            SETTLED_QUIET.as_secs(),
            top >> 20,
            lane.floor >> 20
        ),
        top < lane.floor,
        || {
            format!(
                "{} MiB free at its most: a look could read the volume clear, so nothing below proves the episode stays parked",
                top >> 20
            )
        },
    );
    let rows = lane.rows();
    let (at, standing, what) = match parked {
        Parked::Settled(tab) => (
            rows.iter()
                .position(|r| r.said == "freed" && r.sid == tab.sid),
            "settled",
            "settled (a `settled` row after the credit)",
        ),
        Parked::Out => (
            rows.iter().position(|r| r.said == "out"),
            "spent",
            "out of asks (a `spent` row after the `out` row)",
        ),
    };
    v.check(
        &format!("the next look read the episode {what} and no look ended it (no `ended` row)"),
        at.is_some_and(|i| rows[i..].iter().any(|r| r.said == standing))
            && count(&rows, "ended", "") == 0,
        || format!("rows: {rows:?}"),
    );
    let Counts {
        opened,
        elected,
        typed,
        freed,
        handoff,
        worker,
        out,
    } = *want;
    v.check(
        &format!(
            "nobody else was asked: {opened} `opened`, {elected} `elected`, {typed} `typed`, {freed} `freed`, {handoff} `handoff`, {worker} `worker`, {out} `out` over the whole lane"
        ),
        count(&rows, "opened", "") == opened
            && count(&rows, "elected", "") == elected
            && count(&rows, "step", "typed") == typed
            && count(&rows, "freed", "") == freed
            && count(&rows, "handoff", "") == handoff
            && count(&rows, "worker", "") == worker
            && count(&rows, "out", "") == out,
        || format!("rows: {rows:?}"),
    );
    workers_were(lane, v, &rows, worker);
    Ok(())
}

/// Check that over the whole lane (the ledger's `rows`, every transcript) aterm's disk worker ran
/// `want` times: as many claims (`worker` rows), sessions started (`worker-started` rows) and
/// conversations of its task, so a second worker that starts with no claim of its own, or a
/// second conversation of its task in a session, is found.
fn workers_were(lane: &Lane<'_>, v: &mut Verdict, rows: &[AskRow], want: usize) {
    let claims = count(rows, "worker", "");
    let started = count(rows, "worker-started", "");
    let talks = worker_talks(&crate::workers::transcripts(lane.run)).len();
    v.check(
        &format!(
            "over the whole lane aterm's disk worker ran {want} time(s): {want} `worker` claim(s), {want} `worker-started` row(s), {want} conversation(s) of its task"
        ),
        claims == want && started == want && talks == want,
        || format!("{claims} claim(s), {started} started, {talks} conversation(s) of its task"),
    );
}

/// Check, off every transcript, that each tab received the line `want` times (in the tabs'
/// order) and the disk worker's task never, and that every tab's conversation was read.
fn received_as(lane: &Lane<'_>, v: &mut Verdict, want: &[usize]) {
    let got = lane.received();
    say(&got);
    v.check(
        "every tab's conversation was read (a transcript whose first prompt holds its token)",
        got.unread().is_empty(),
        || format!("none for {:?}", got.unread()),
    );
    v.check(
        &format!(
            "over the whole lane, read off every transcript, the disk line reached {}, and no conversation of no tab's, and the disk worker's task no tab's conversation",
            wanted(&lane.tabs, want)
        ),
        got.exactly(want),
        || format!("{got:?}"),
    );
}

/// `aterm harness disk why`, printed: its exit and its lines.
fn why(lane: &Lane<'_>) -> Result<(i32, Vec<String>), Fail> {
    let why = lane.door.harness(&["disk", "why"])?;
    println!("  why       (exit {})", why.code);
    for l in why.stdout.lines() {
        println!("    | {l}");
    }
    if !why.stderr.trim().is_empty() {
        println!("    ! {}", why.stderr.trim());
    }
    Ok((why.code, why.stdout.lines().map(str::to_owned).collect()))
}

/// Check that `why` (its exit and lines) names `tab`'s freeing (its `freed` row), and with
/// `settled` the lease's episode settled in its name.
fn why_credits(v: &mut Verdict, why: &(i32, Vec<String>), tab: &Tab, settled: bool) {
    let (code, lines) = why;
    let row = format!(" freed @{} ", tab.sid);
    let head = format!("@{} freed ", tab.sid);
    v.check(
        &format!(
            "`aterm harness disk why` names {}'s freeing (@{}){}",
            tab.name,
            tab.sid,
            if settled {
                ", the episode settled in its name"
            } else {
                ""
            }
        ),
        *code == 0
            && lines.iter().any(|l| l.contains(&row))
            && (!settled
                || lines
                    .iter()
                    .any(|l| l.contains("settled at") && l.contains(&head))),
        || lines.join("\n"),
    );
}

/// `clear`, past the head: the asked tab's cleanup lifts the volume past its floor and the margin,
/// the ledger credits it and a later look ends the episode; the volume filled short again within
/// the cooldown, a look opens no episode and claims aterm's disk worker instead, which ends with
/// no tab typed anything, and nobody gets a second line.
fn clear(lane: &Lane<'_>, v: &mut Verdict, h: &Head) -> Result<(), Fail> {
    let biggest = &lane.tabs[0];
    let (t_rise, _) = rise(lane, v, biggest, h.free_before, false)?;
    let settled = credit(lane, v, biggest, 0)?;
    let t_settle = Instant::now();
    let ended = wait_for(TICK * 2 + SLACK, || {
        lane.show();
        lane.rows()
            .iter()
            .skip_while(|r| !(r.said == "freed" && r.sid == biggest.sid))
            .any(|r| r.said == "ended")
    })?;
    v.check(
        "a later look read the volume clear and ended the episode (an `ended` row after the credit)",
        ended,
        || format!("rows: {:?}", lane.rows()),
    );
    let t_ended = Instant::now();
    // The volume short again within the cooldown of the line: a look reads it so, aterm's own pass
    // spent, and opens no episode, so nobody is asked again. A second line would come here, if
    // anywhere: an episode that opened would elect within a round and type at the next break.
    let clear = lane.free();
    if clear > lane.laid_free {
        fill(&lane.home.join("ballast/refill"), clear - lane.laid_free)?;
    }
    let short = lane.free();
    let refilled_at = lane.rows().len();
    println!(
        "  refilled  {} MiB -> {} MiB free (floor {} MiB) {}",
        clear >> 20,
        short >> 20,
        lane.floor >> 20,
        lane.since(Instant::now())
    );
    v.check(
        "the volume is filled below its floor again",
        short < lane.floor,
        || format!("{} MiB free", short >> 20),
    );
    let since_refill = |said: &str| {
        lane.rows()
            .into_iter()
            .skip(refilled_at)
            .filter(|r| r.said == said)
            .count()
    };
    let cooled = wait_for(TICK * 2 + SLACK, || {
        lane.show();
        since_refill("cooldown") > 0 || since_refill("opened") > 0
    })?;
    v.check(
        "a look read the volume short again and opened no episode, the line within the cooldown (a `cooldown` row)",
        cooled && since_refill("cooldown") > 0 && since_refill("opened") == 0,
        || {
            let rows = lane.rows();
            format!(
                "rows since the refill: {:?}",
                &rows[refilled_at.min(rows.len())..]
            )
        },
    );
    let t_cooled = Instant::now();
    // The line cooldown spares the tabs a second line, not the volume aterm's disk worker.
    worker(
        lane,
        v,
        refilled_at,
        "no tab may be asked within the line's cooldown",
        &[1, 0, 0],
    )?;
    let t_worked = Instant::now();
    lane.watch(QUIET)?;
    let rows = lane.rows();
    let still = lane.free();
    v.check(
        &format!(
            "no second episode, line or worker over {} s more of the volume short ({} MiB free at the end): one `opened`, one `elected`, one `typed`, one `worker`",
            QUIET.as_secs(),
            still >> 20
        ),
        still < lane.floor
            && count(&rows, "opened", "") == 1
            && count(&rows, "elected", "") == 1
            && count(&rows, "step", "typed") == 1
            && count(&rows, "worker", "") == 1,
        || format!("rows: {rows:?}"),
    );
    workers_were(lane, v, &rows, 1);
    received_as(lane, v, &[1, 0, 0]);
    let why = why(lane)?;
    why_credits(v, &why, biggest, false);
    let secs = |a: Instant, b: Instant| b.saturating_duration_since(a).as_secs();
    println!(
        "  summary   opened {} s after the tabs were up, elected {} s later, the line {} s after the election, the rise {} s after the line, {}, ended {} s later; refilled short, {} s to the look that opened nothing, the worker over {} s later, then {} s quiet",
        secs(lane.t0, h.t_open),
        secs(h.t_open, h.t_elected),
        secs(h.t_elected, h.t_line),
        secs(h.t_line, t_rise),
        if settled {
            format!("the credit {} s after the rise", secs(t_rise, t_settle))
        } else {
            format!("no credit within {} s of the rise", secs(t_rise, t_settle))
        },
        secs(t_settle, t_ended),
        secs(t_ended, t_cooled),
        secs(t_cooled, t_worked),
        QUIET.as_secs()
    );
    Ok(())
}

/// `partial`, past the head: the asked tab's cleanup frees past [`ask::FREED_MIN`] with the volume
/// still below its floor; the ledger credits it, the episode stays settled and nobody else is
/// asked over three more looks of a short volume.
fn partial(lane: &Lane<'_>, v: &mut Verdict, h: &Head) -> Result<(), Fail> {
    let asked = &lane.tabs[0];
    let (t_rise, _) = rise(lane, v, asked, h.free_before, true)?;
    let credited = credit(lane, v, asked, 0)?;
    let t_settle = Instant::now();
    if !credited {
        return Ok(());
    }
    parked_quiet(
        lane,
        v,
        Parked::Settled(asked),
        &Counts {
            opened: 1,
            elected: 1,
            typed: 1,
            freed: 1,
            handoff: 0,
            worker: 0,
            out: 0,
        },
    )?;
    received_as(lane, v, &[1, 0, 0]);
    let why = why(lane)?;
    why_credits(v, &why, asked, true);
    let secs = |a: Instant, b: Instant| b.saturating_duration_since(a).as_secs();
    println!(
        "  summary   opened {} s after the tabs were up, elected {} s later, the line {} s after the election, the rise {} s after the line, the credit {} s after the rise, then {} s settled and short",
        secs(lane.t0, h.t_open),
        secs(h.t_open, h.t_elected),
        secs(h.t_elected, h.t_line),
        secs(h.t_line, t_rise),
        secs(t_rise, t_settle),
        SETTLED_QUIET.as_secs()
    );
    Ok(())
}

/// How the ledger reads a silent tab's miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissRow {
    /// Its ask handed on to one other session: a `handoff` row in its name.
    Handoff,
    /// The episode out, its lines spent: an `out` row, the ask handed to nobody.
    Out,
}

impl MissRow {
    const fn word(self) -> &'static str {
        match self {
            MissRow::Handoff => "handoff",
            MissRow::Out => "out",
        }
    }
}

/// Silent tab `tab`, whose line went at `line.0` with `line.1` free: check that it answered with
/// words alone, and that the lease holds it asked for [`ask::ANSWER_MS`]; wait (a hang detector of
/// its answer window, two looks and slack, from its line) for a ledger row past the first `skip`
/// that reads it missed, its ask handed off (a `handoff` row in its name) or the episode out (an
/// `out` row); and check that it cleaned nothing over its answer window, that the row is `want`'s,
/// says `nothing-freed` and came no earlier than the window's end with no credit before it, and
/// that by then the tabs had received the lines `lines` counts (read off every transcript) and
/// the ledger typed no other. Where that row lies and when it was read, or `None` when no row of
/// `want`'s came.
#[allow(clippy::too_many_lines)]
fn missed(
    lane: &Lane<'_>,
    v: &mut Verdict,
    tab: &Tab,
    line: (Instant, u64),
    skip: usize,
    want: MissRow,
    lines: &[usize],
) -> Result<Option<(usize, Instant)>, Fail> {
    let (t_line, free_before) = line;
    let answered = tab_shows(lane.door, &tab.sid, "Noted: I am keeping", RISE + SLACK)?;
    v.check(
        &format!(
            "{} answered its line with words alone, its turn over",
            tab.name
        ),
        answered,
        || lane.door.out(&[&format!("@{}", tab.sid), "text"]),
    );
    let asked = lane.lease().volumes.iter().find_map(|e| match &e.phase {
        ask::Phase::Asked {
            pick,
            typed_ms,
            until_ms,
            ..
        } if pick.sid == tab.sid => Some((*typed_ms, *until_ms)),
        _ => None,
    });
    let rfc = |ms: i64| aterm_agent::harness::usage::rfc3339_utc(ms.div_euclid(1000));
    match asked {
        Some((typed, until)) => println!(
            "  asked     @{} at {}: its answer window ends {} (ANSWER_MS {} s)",
            tab.sid,
            rfc(typed),
            rfc(until),
            ask::ANSWER_MS / 1000
        ),
        None => println!("  asked     the lease holds no asked phase of @{}", tab.sid),
    }
    v.check(
        &format!(
            "the lease holds {} asked, its answer window ANSWER_MS from its line",
            tab.name
        ),
        asked.is_some_and(|(typed, until)| until - typed == ask::ANSWER_MS),
        || format!("{:?}", lane.lease()),
    );
    // A hang detector of the answer window, two looks and slack, from the line.
    let answer = Duration::from_millis(ask::ANSWER_MS.unsigned_abs());
    let within = answer
        .saturating_sub(t_line.elapsed())
        .saturating_add(TICK * 2 + SLACK);
    let mut top = free_before;
    let mut miss: Option<(usize, AskRow)> = None;
    wait_for(within, || {
        lane.show();
        top = top.max(lane.free());
        miss = lane
            .rows()
            .into_iter()
            .enumerate()
            .skip(skip)
            .find(|(_, r)| (r.said == "handoff" && r.sid == tab.sid) || r.said == "out");
        miss.is_some()
    })?;
    let t_miss = Instant::now();
    // Read at once: a next line comes a round of nominations later at the earliest.
    let got = lane.received();
    say(&got);
    let rows = lane.rows();
    let at = miss.as_ref().map_or(rows.len(), |(i, _)| *i);
    let row = miss.map(|(_, r)| r);
    v.check(
        &format!(
            "{} cleaned nothing over its answer window: its target/ still there, free space never {} MiB past its line's ({} MiB at most)",
            tab.name,
            ask::FREED_MIN >> 20,
            top >> 20
        ),
        tab.dir.join("target").exists() && top < free_before + ask::FREED_MIN,
        || {
            format!(
                "target/ there: {}, {} MiB at the line, {} MiB at most since",
                tab.dir.join("target").exists(),
                free_before >> 20,
                top >> 20
            )
        },
    );
    let ts = row
        .as_ref()
        .and_then(|r| aterm_types::rfc3339::parse_utc(&r.ts));
    if let (Some(r), Some(s), Some((typed, _))) = (&row, ts, asked) {
        println!(
            "  {:<9} @{} {} s after its line {}",
            r.said,
            tab.sid,
            (s * 1000 - typed) / 1000,
            lane.since(t_miss)
        );
    }
    let says = match want {
        MissRow::Handoff => format!(
            "handed {}'s ask off as `nothing-freed` (a `handoff` row in its name)",
            tab.name
        ),
        MissRow::Out => format!(
            "ran the episode out as `nothing-freed` (an `out` row, its {} lines spent), handing {}'s ask to nobody",
            ask::MAX_ASKS,
            tab.name
        ),
    };
    v.check(
        &format!("the ledger {says}, not before its answer window ended, with no credit before it"),
        row.as_ref()
            .is_some_and(|r| r.said == want.word() && r.why == Miss::NothingFreed.word())
            && ts
                .zip(asked)
                .is_some_and(|(s, (_, until))| s * 1000 + 999 >= until)
            && !rows[..at].iter().any(|r| r.said == "freed"),
        || {
            format!(
                "within {} s of the line; read: {row:?}; asked: {asked:?}; rows: {rows:?}",
                (answer + TICK * 2 + SLACK).as_secs()
            )
        },
    );
    let typed: usize = lines.iter().sum();
    v.check(
        &format!(
            "nobody else was typed anything before the `{}` row: the line reached {}, read off every transcript, and the ledger typed {typed}",
            want.word(),
            wanted(&lane.tabs, lines)
        ),
        got.exactly(lines) && count(&rows[..at], "step", "typed") == typed,
        || format!("{got:?}; rows: {rows:?}"),
    );
    Ok(row.filter(|r| r.said == want.word()).map(|_| (at, t_miss)))
}

/// The round after the first tab's hand-off: the second tab's line.
struct Next {
    /// When aterm's disk worker was read over.
    t_worked: Instant,
    t_elected: Instant,
    t_line: Instant,
    /// Free space as the election was read, before the line.
    free_before: u64,
    /// Where the rounds read on from in the ledger: past the worker's `worker-over`.
    resume: usize,
}

/// After the first tab's ask was handed off as `nothing-freed` (its row at `at`): aterm's disk
/// worker comes first and the rounds wait on it ([`worker`]); then the next round elects the
/// second tab, whose checkout holds the most of the tabs not yet asked, and its line, the rule's,
/// names its own build. `None`: a step the rest stands on did not come.
fn handed_on(lane: &Lane<'_>, v: &mut Verdict, at: usize) -> Result<Option<Next>, Fail> {
    let (first, second) = (&lane.tabs[0], &lane.tabs[1]);
    let resume = worker(
        lane,
        v,
        at,
        &format!("{} freed nothing in its answer window", first.name),
        &[1, 0, 0],
    )?
    .map_or(at, |w| w.resume);
    let t_worked = Instant::now();
    let nominate = Duration::from_millis(ask::NOMINATE_MS.unsigned_abs());
    let pick = lane
        .wait_row(nominate + TICK * 2 + SLACK, resume, |r| r.said == "elected")?
        .map(|r| r.sid);
    v.check(
        &format!(
            "the round after the worker elected {} (@{}), whose checkout holds the most of the tabs not yet asked",
            second.name, second.sid
        ),
        pick.as_deref() == Some(second.sid.as_str()),
        || format!("pick={pick:?}; rows: {:?}", lane.rows()),
    );
    let Some(pick) = pick else {
        return Ok(None);
    };
    let t_elected = Instant::now();
    println!("  elected   @{pick} {}", lane.since(t_elected));
    let free_before = lane.free();
    let typed = wait_for(TICK * 2 + SLACK, || {
        lane.show();
        lane.received().total() > 1
    })?;
    let t_line = Instant::now();
    let got = lane.received();
    say(&got);
    v.check("a second line reached a tab's conversation", typed, || {
        format!(
            "none within {} s of the second election",
            (TICK * 2 + SLACK).as_secs()
        )
    });
    if !typed {
        return Ok(None);
    }
    line_is_the_rules(v, got.first_of(1).unwrap_or_default(), second);
    Ok(Some(Next {
        t_worked,
        t_elected,
        t_line,
        free_before,
        resume,
    }))
}

/// Check that `why` (its exit and lines) names `tab`'s hand-off as nothing-freed.
fn why_hands_off(v: &mut Verdict, why: &(i32, Vec<String>), tab: &Tab) {
    let gave = format!(" handoff @{} ", tab.sid);
    v.check(
        &format!(
            "`aterm harness disk why` names {}'s hand-off as nothing-freed",
            tab.name
        ),
        why.0 == 0
            && why
                .1
                .iter()
                .any(|l| l.contains(&gave) && l.contains(Miss::NothingFreed.word())),
        || why.1.join("\n"),
    );
}

/// `unanswered`, past the head: the asked tab answers with words alone and frees nothing; once its
/// answer window ([`ask::ANSWER_MS`]) passes, the ledger hands its ask off, and nobody else has
/// been typed anything; aterm's disk worker is claimed and ends with no tab typed anything; the
/// next round elects the second tab, whose line names its own build and whose cleanup settles the
/// episode with the volume short; the third tab is never asked and the first never asked again.
fn unanswered(lane: &Lane<'_>, v: &mut Verdict, h: &Head) -> Result<(), Fail> {
    let (first, second) = (&lane.tabs[0], &lane.tabs[1]);
    let line = (h.t_line, h.free_before);
    let Some((at, t_handoff)) = missed(lane, v, first, line, 0, MissRow::Handoff, &[1, 0, 0])?
    else {
        return Ok(());
    };
    let Some(n) = handed_on(lane, v, at)? else {
        return Ok(());
    };
    let (t_rise, _) = rise(lane, v, second, n.free_before, true)?;
    let credited = credit(lane, v, second, at)?;
    let t_settle = Instant::now();
    if !credited {
        return Ok(());
    }
    parked_quiet(
        lane,
        v,
        Parked::Settled(second),
        &Counts {
            opened: 1,
            elected: 2,
            typed: 2,
            freed: 1,
            handoff: 1,
            worker: 1,
            out: 0,
        },
    )?;
    received_as(lane, v, &[1, 1, 0]);
    let why = why(lane)?;
    why_credits(v, &why, second, true);
    why_hands_off(v, &why, first);
    let secs = |a: Instant, b: Instant| b.saturating_duration_since(a).as_secs();
    println!(
        "  summary   opened {} s after the tabs were up, elected {} s later, the line {} s after the election; the hand-off {} s after the line, the worker over {} s later, the second election {} s after that, its line {} s after it, the rise {} s after that, the credit {} s after the rise, then {} s settled and short",
        secs(lane.t0, h.t_open),
        secs(h.t_open, h.t_elected),
        secs(h.t_elected, h.t_line),
        secs(h.t_line, t_handoff),
        secs(t_handoff, n.t_worked),
        secs(n.t_worked, n.t_elected),
        secs(n.t_elected, n.t_line),
        secs(n.t_line, t_rise),
        secs(t_rise, t_settle),
        SETTLED_QUIET.as_secs()
    );
    Ok(())
}

/// `exhausted`, past the head: as `unanswered` up to the second tab's line, which that tab answers
/// with words alone too; once its answer window passes the episode's [`ask::MAX_ASKS`] lines are
/// spent, so the ledger runs it out instead of handing the ask on, no second worker is claimed
/// ([`ask::WORKER_COOLDOWN_MS`]), the episode stays out over three more looks of a short volume,
/// and the third tab is never asked, nor either asked tab a second time.
fn exhausted(lane: &Lane<'_>, v: &mut Verdict, h: &Head) -> Result<(), Fail> {
    let (first, second) = (&lane.tabs[0], &lane.tabs[1]);
    let line = (h.t_line, h.free_before);
    let Some((at, t_handoff)) = missed(lane, v, first, line, 0, MissRow::Handoff, &[1, 0, 0])?
    else {
        return Ok(());
    };
    let Some(n) = handed_on(lane, v, at)? else {
        return Ok(());
    };
    let line = (n.t_line, n.free_before);
    let Some((_, t_out)) = missed(lane, v, second, line, n.resume, MissRow::Out, &[1, 1, 0])?
    else {
        return Ok(());
    };
    parked_quiet(
        lane,
        v,
        Parked::Out,
        &Counts {
            opened: 1,
            elected: 2,
            typed: 2,
            freed: 0,
            handoff: 1,
            worker: 1,
            out: 1,
        },
    )?;
    received_as(lane, v, &[1, 1, 0]);
    let why = why(lane)?;
    why_hands_off(v, &why, first);
    let (code, lines) = &why;
    let cap = ask::MAX_ASKS;
    let picked = format!("lines {cap}/{cap}; picked @{}, @{};", first.sid, second.sid);
    v.check(
        &format!(
            "`aterm harness disk why` reads the episode out (nothing-freed), its lines {cap}/{cap}, {} and {} its only picks",
            first.name, second.name
        ),
        *code == 0
            && lines
                .iter()
                .any(|l| l.contains(": out at ") && l.contains(" (nothing-freed): "))
            && lines.iter().any(|l| l.contains(&picked)),
        || lines.join("\n"),
    );
    let secs = |a: Instant, b: Instant| b.saturating_duration_since(a).as_secs();
    println!(
        "  summary   opened {} s after the tabs were up, elected {} s later, the line {} s after the election; the hand-off {} s after the line, the worker over {} s later, the second election {} s after that, its line {} s after it, out {} s after that, then {} s out and short",
        secs(lane.t0, h.t_open),
        secs(h.t_open, h.t_elected),
        secs(h.t_elected, h.t_line),
        secs(h.t_line, t_handoff),
        secs(t_handoff, n.t_worked),
        secs(n.t_worked, n.t_elected),
        secs(n.t_elected, n.t_line),
        secs(n.t_line, t_out),
        SETTLED_QUIET.as_secs()
    );
    Ok(())
}

/// `text` with all whitespace dropped, so words a terminal wrapped read as they were written.
fn squashed(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The bytes the tree at `p` holds (removing it would release), read without following a link.
fn tree_bytes(p: &Path) -> u64 {
    aterm_agent::harness::disk::target::cache_bytes(p, None).0
}

/// What the worker must leave whole ([`dedicated`]): the ballast, which is no cache, and every
/// tab's build, each with the bytes it holds now.
fn kept_now(lane: &Lane<'_>) -> Vec<(PathBuf, u64)> {
    std::iter::once(lane.home.join(BALLAST))
        .chain(lane.tabs.iter().map(|t| t.dir.join("target")))
        .map(|p| {
            let b = tree_bytes(&p);
            (p, b)
        })
        .collect()
}

/// `worker`, from the tabs up, each holding its draft, and the home laid short: an episode opens,
/// its round waits on the drafted tabs, and a look claims aterm's disk worker ([`worker`]), which
/// starts where no Claude Code ran a command, so no shell snapshot stands: its first box, a dry
/// run before its `pwd`, is declined with that reason and the remedy, `pwd`
/// ([`aterm_agent::supervise::policy::DISK_WORKER_SHELL_UNREAD`]), never the fence's text, and
/// once its `pwd` ran its boxes are proven. Its fence declines its `rm -rf` of the cache
/// ([`CACHE`]) with the fence's reason ([`aterm_agent::supervise::policy::DISK_WORKER_DECLINE`]),
/// the cache whole after it (the dry run after the `rm` names it, and the verb's `removing` row
/// weighs all of it, and no more: its link to the ballast is not followed); `aterm harness disk
/// free` of the ballast and a tab's build by their whole paths, which Claude Code draws on two
/// rows, is declined with that reason and its remedy
/// ([`aterm_agent::supervise::policy::DISK_WORKER_WRAPPED`]), and written under `~` on one row is
/// approved and refuses each, journalled so; of the cache, it removes it, approved under the
/// fence's rule and journalled in the worker's name, and the volume rises by its size and no
/// more, the ballast and every tab's build whole. Over three more looks of a volume still short
/// no second worker ([`ask::WORKER_COOLDOWN_MS`]), session or conversation of its task and no
/// line come and nothing is typed into the worker; `aterm harness disk why` says what the worker
/// freed; and no tab was elected, typed anything, or lost its draft.
#[allow(clippy::too_many_lines)]
fn dedicated(lane: &Lane<'_>, v: &mut Verdict) -> Result<(), Fail> {
    let Some(planted) = lane.planted.as_ref() else {
        return Err(Fail::die("disk-ask: the worker scenario laid no cache"));
    };
    let (cache, cache_bytes) = (planted.cache.as_path(), planted.bytes);
    let within = FIRST_LOOK + TICK * 3 + SLACK;
    let opened = lane.wait_row(within, 0, |r| r.said == "opened")?.is_some();
    v.check(
        "an episode opened once aterm's own pass was spent",
        opened,
        || {
            format!(
                "no `opened` row within {} s of the home laid short; rows: {:?}",
                within.as_secs(),
                lane.rows()
            )
        },
    );
    if !opened {
        return Ok(());
    }
    let t_open = Instant::now();
    println!("  opened    {}", lane.since(t_open));
    // aterm's own pass is spent: what the worker must leave holds what it holds from here on.
    let kept = kept_now(lane);
    for (p, b) in &kept {
        println!("  kept      {} {} MiB", p.display(), b >> 20);
    }
    let nominate = Duration::from_millis(ask::NOMINATE_MS.unsigned_abs());
    let waiting = lane.wait_row(nominate + TICK * 2 + SLACK, 0, |r| {
        r.said == "waiting" || r.said == "elected"
    })?;
    if let Some(w) = &waiting {
        println!("  {:<9} {} {}", w.said, w.text, lane.since(Instant::now()));
    }
    v.check(
        "the round waits on the tabs, each held by its draft (a `waiting` row naming every tab and its draft, no election)",
        waiting.as_ref().is_some_and(|w| {
            w.said == "waiting"
                && w.text.contains("draft")
                && lane
                    .tabs
                    .iter()
                    .all(|t| w.text.contains(&format!("@{}", t.sid)))
        }),
        || format!("{waiting:?}; rows: {:?}", lane.rows()),
    );
    let t_waiting = Instant::now();
    let none = vec![0; lane.tabs.len()];
    let Some(w) = worker(
        lane,
        v,
        0,
        "every tab holds a draft, so the round waits on them",
        &none,
    )?
    else {
        return Ok(());
    };
    let t_worked = Instant::now();
    let Some(wsid) = w.sid.clone() else {
        return Ok(());
    };
    let all = crate::workers::transcripts(lane.run);
    let talk = worker_talk(&all);
    let ledger = decided(lane.run, &wsid);
    for d in &ledger {
        println!(
            "  decided   @{wsid} {} {} {}",
            d.rule,
            d.decision,
            d.command.chars().take(160).collect::<String>()
        );
    }
    let rule = aterm_agent::supervise::policy::RULE_DISK_WORKER;
    let approved = aterm_agent::supervise::approvals::Outcome::Approved.word();
    let refused = aterm_agent::supervise::approvals::Outcome::Declined.word();
    let fence = aterm_agent::supervise::policy::DISK_WORKER_DECLINE;
    let unread = aterm_agent::supervise::policy::decline_text(
        "this command",
        aterm_agent::supervise::policy::DISK_WORKER_SHELL_UNREAD,
        aterm_agent::supervise::policy::DISK_WORKER_FIRST_BASH,
    );
    // Its first box: the dry run before any command of its ran, no snapshot standing.
    let dry = results_of(talk, FREE_DRY);
    let first_dry = ledger.iter().find(|d| boxed(d, FREE_DRY));
    v.check(
        &format!(
            "the worker's dry run before its `{FIRST_BASH}`, with no shell snapshot standing, was declined by its fence ({rule}) with that reason and its remedy, `{FIRST_BASH}`, not the fence's text, and its result says so"
        ),
        first_dry.is_some_and(|d| {
            d.rule == rule && d.decision == refused && d.reason == unread && !d.reason.contains(fence)
        }) && dry
            .first()
            .is_some_and(|r| squashed(r).contains(&squashed(&unread))),
        || format!("its first dry-run row {first_dry:?}; the dry run's results {dry:?}"),
    );
    let rm = rm_cache(cache);
    let rm_results = results_of(talk, &rm);
    let rm_declined = ledger
        .iter()
        .any(|d| d.rule == rule && d.decision == refused && boxed(d, &rm) && d.reason == fence);
    v.check(
        &format!(
            "the worker's `rm -rf` of the cache was declined by its fence ({rule}), its result the fence's reason"
        ),
        rm_declined
            && !rm_results.is_empty()
            && rm_results
                .iter()
                .all(|r| squashed(r).contains(&squashed(fence))),
        || format!("its ledger's {rule} declines: {rm_declined}; the `rm`'s results: {rm_results:?}"),
    );
    let would = format!("would remove {}", cache.display());
    let frees = lane.free_rows();
    let mine = |action: &str, path: &Path| {
        frees
            .iter()
            .find(|r| r.action == action && r.sid == wsid && r.path == path.display().to_string())
    };
    let removing = mine("removing", cache).and_then(|r| r.bytes);
    // The cache's own bytes and its link: a count that followed the link would weigh the ballast.
    let near = cache_bytes / 64;
    v.check(
        "the cache was still there, whole, after the declined `rm`: the dry run after it would remove it, and the verb's `removing` row weighs all of it and no more (its link to the ballast not followed)",
        dry.iter().any(|r| r.contains(&would))
            && removing.is_some_and(|b| b + near >= cache_bytes && b <= cache_bytes + near),
        || {
            format!(
                "the dry run's results: {dry:?}; the `removing` row's bytes: {removing:?} of {} MiB laid",
                cache_bytes >> 20
            )
        },
    );
    let outside: Vec<&Path> = planted.outside.iter().map(PathBuf::as_path).collect();
    // By their whole paths the two outside paths fill more than the box's row: Claude Code draws
    // the command on two, which read as lines make a second command.
    let free_whole = free_of(&outside);
    let wrapped = aterm_agent::supervise::policy::decline_text(
        "this command",
        aterm_agent::supervise::policy::DISK_WORKER_WRAPPED,
        aterm_agent::supervise::policy::DISK_WORKER_ONE_ROW,
    );
    let whole_results = results_of(talk, &free_whole);
    let whole_row = ledger.iter().find(|d| boxed(d, &free_whole));
    v.check(
        &format!(
            "`{free_whole}` ({} characters), which Claude Code drew on more than one row, was declined by its fence ({rule}) with that reason and its remedy, one row with the home written `~`, not the fence's text, and its result says so",
            free_whole.chars().count()
        ),
        whole_row.is_some_and(|d| {
            d.rule == rule && d.decision == refused && d.reason == wrapped && !d.reason.contains(fence)
        }) && whole_results
            .first()
            .is_some_and(|r| squashed(r).contains(&squashed(&wrapped))),
        || format!("its ledger row {whole_row:?}; its results {whole_results:?}"),
    );
    let free_outside = free_in_home(planted);
    let outside_results = results_of(talk, &free_outside);
    let outside_approved = ledger
        .iter()
        .any(|d| d.rule == rule && d.decision == approved && boxed(d, &free_outside));
    let refused_rows: Vec<Option<&FreeRow>> = outside.iter().map(|p| mine("refused", p)).collect();
    v.check(
        &format!(
            "`{free_outside}` ran, approved under {rule}, and the verb refused each path, no cache: its result says so, the ledger journals a `refused` row of each in the worker's name, and no `removing` row of any"
        ),
        outside_approved
            && outside.iter().all(|p| {
                outside_results
                    .iter()
                    .any(|r| r.contains(&format!("refused {}", p.display())))
            })
            && refused_rows.iter().all(Option::is_some)
            && outside.iter().all(|p| mine("removing", p).is_none()),
        || {
            format!(
                "approved: {outside_approved}; its results: {outside_results:?}; disk-free rows: {frees:?}"
            )
        },
    );
    let free_cmd = free_of(&[cache]);
    let freed_results = results_of(talk, &free_cmd);
    let verb_approved = ledger
        .iter()
        .any(|d| d.rule == rule && d.decision == approved && boxed(d, &free_cmd));
    let after = lane.free();
    let gone = !cache.exists();
    println!(
        "  freed     the cache: {} MiB free at the claim -> {} MiB now (floor {} MiB), its folder there: {}",
        w.free_at_claim >> 20,
        after >> 20,
        lane.floor >> 20,
        !gone
    );
    v.check(
        &format!(
            "`aterm harness disk free` of the cache ran, approved under {rule}, and removed it: its result says so, the ledger journals its `removing` and `removed` rows in the worker's name, and the cache is gone"
        ),
        verb_approved
            && freed_results
                .iter()
                .any(|r| r.contains(&format!("removed {}", cache.display())))
            && removing.is_some()
            && mine("removed", cache).is_some()
            && gone,
        || {
            format!(
                "approved: {verb_approved}; its results: {freed_results:?}; disk-free rows: {frees:?}; the cache there: {}",
                !gone
            )
        },
    );
    let whole: Vec<(&PathBuf, u64, u64)> = kept
        .iter()
        .map(|(p, then)| (p, *then, tree_bytes(p)))
        .collect();
    for (p, then, now) in &whole {
        println!(
            "  kept      {} {} MiB -> {} MiB",
            p.display(),
            then >> 20,
            now >> 20
        );
    }
    v.check(
        "the ballast and every tab's build are whole after the worker: each still there, holding what it held when the episode opened",
        whole
            .iter()
            .all(|(p, then, now)| p.exists() && *then > 0 && now * 100 >= then * 99),
        || format!("{whole:?}"),
    );
    let rose = w.over.rose.unwrap_or(0);
    // What the volume may gain past the cache: nothing else is removed, and what the sessions
    // write only takes space.
    let past = SPARE / 4;
    v.check(
        &format!(
            "the volume rose by the cache's {} GiB and no more (within {} MiB): read since the claim, and as the worker's `worker-over` row says",
            cache_bytes >> 30,
            past >> 20
        ),
        after >= w.free_at_claim + cache_bytes * 9 / 10
            && after <= w.free_at_claim + cache_bytes + past
            && rose >= cache_bytes * 9 / 10
            && rose <= cache_bytes + past,
        || {
            format!(
                "{} MiB at the claim, {} MiB now; the row's rise {} MiB",
                w.free_at_claim >> 20,
                after >> 20,
                rose >> 20
            )
        },
    );
    let top = lane.watch(SETTLED_QUIET)?;
    let rows = lane.rows();
    let since = &rows[w.resume.min(rows.len())..];
    v.check(
        &format!(
            "over {} s of looks after the worker, the volume still below its floor ({} MiB at most, floor {} MiB), no second worker, no election, no line and no end",
            SETTLED_QUIET.as_secs(),
            top >> 20,
            lane.floor >> 20
        ),
        top < lane.floor
            && count(since, "worker", "") == 0
            && count(since, "worker-started", "") == 0
            && count(since, "elected", "") == 0
            && count(since, "step", "typed") == 0
            && count(&rows, "ended", "") == 0,
        || format!("rows since the worker was over: {since:?}"),
    );
    workers_were(lane, v, &rows, 1);
    let after_quiet = reported(lane, &wsid);
    v.check(
        "nothing was typed into aterm's disk worker after its report, through the looks after it",
        after_quiet.report.is_some() && after_quiet.typed_after.is_empty(),
        || format!("typed after its report: {:?}", after_quiet.typed_after),
    );
    let why = why(lane)?;
    let head = format!("worker: @{wsid} over at ");
    let tail = format!(
        "(finished); free space rose {} while it ran",
        aterm_agent::harness::disk::human_bytes(rose)
    );
    v.check(
        "`aterm harness disk why` describes the worker: its session, over as finished, and the space it freed",
        why.0 == 0
            && why
                .1
                .iter()
                .any(|l| l.contains(&head) && l.contains(&tail)),
        || why.1.join("\n"),
    );
    received_as(lane, v, &none);
    let typed = aterm_agent::supervise::approvals::Outcome::Typed.word();
    for t in &lane.tabs {
        let at = format!("@{}", t.sid);
        let draft = t.draft.as_deref().unwrap_or_default();
        let composer = crate::screen::composer_row("claude", &lane.door.out(&[&at, "text"]));
        let said: Vec<String> = all
            .iter()
            .filter(|(_, rows)| {
                crate::workers::first_prompt(rows).is_some_and(|p| p.text.contains(&t.token))
            })
            .flat_map(|(_, rows)| rows.iter())
            .filter(|r| r.kind == "user" && r.result.is_none() && !r.text.trim().is_empty())
            .map(|r| r.text.clone())
            .collect();
        let typed_in: Vec<Decided> = decided(lane.run, &t.sid)
            .into_iter()
            .filter(|d| d.decision == typed)
            .collect();
        v.check(
            &format!(
                "{}'s draft still stands in its composer, unsent, and nothing was typed into it: its conversation holds its task alone, its ledger no `typed` row",
                t.name
            ),
            composer.as_deref() == Some(draft)
                && said.len() == 1
                && said.iter().all(|s| s.contains(&t.token))
                && typed_in.is_empty(),
            || format!("composer {composer:?} (draft {draft:?}); its prompts {said:?}; typed {typed_in:?}"),
        );
    }
    v.check(
        "over the whole lane one episode, no tab elected or typed a line, one worker",
        count(&rows, "opened", "") == 1
            && count(&rows, "elected", "") == 0
            && count(&rows, "step", "typed") == 0
            && count(&rows, "worker", "") == 1,
        || format!("rows: {rows:?}"),
    );
    let secs = |a: Instant, b: Instant| b.saturating_duration_since(a).as_secs();
    println!(
        "  summary   opened {} s after the home was laid short, the round waited {} s later, the worker over {} s after that, then {} s quiet",
        secs(lane.t0, t_open),
        secs(t_open, t_waiting),
        secs(t_waiting, t_worked),
        SETTLED_QUIET.as_secs()
    );
    Ok(())
}

/// What the lane leaves unsent in tab `name`'s composer ([`Scenario::drafted`]).
fn draft_of(name: &str) -> String {
    format!("next: rerun the {name} tests once the build is back")
}

/// Lay the home short: each tab's build (a recent one, in its `target/debug/deps`), the cache
/// where the scenario plants one, then ballast ([`BALLAST`]) down to [`Geometry::laid_free`], and
/// in the cache a link to the ballast ([`ESCAPE`]); and check that the volume reads below its
/// floor and past the emergency line.
fn lay_short(
    home: &Path,
    g: &Geometry,
    tabs: &[Tab],
    planted: Option<&Planted>,
    v: &mut Verdict,
) -> Result<(), Fail> {
    for t in tabs {
        lay_target_sized(&t.dir.join("target"), t.gib * GIB, 1 << 20)?;
    }
    if let Some(p) = planted {
        fill(&p.cache.join("blobs"), p.bytes / 2)?;
        fill(&p.cache.join("index"), p.bytes - p.bytes / 2)?;
    }
    let free = free_on(home).ok_or("the image's free space could not be read")?;
    let ballast = home.join(BALLAST);
    if free > g.laid_free {
        fill(&ballast.join("fill"), free - g.laid_free)?;
    }
    if let Some(p) = planted {
        let link = p.cache.join(ESCAPE);
        std::os::unix::fs::symlink(&ballast, &link)
            .map_err(|e| Fail::die(format!("disk-ask: the link {}: {e}", link.display())))?;
    }
    let laid = free_on(home).unwrap_or(0);
    println!(
        "  laid      {} (each a recent build in target/debug/deps){}, ballast to {} MiB free (floor {} MiB){}",
        tabs.iter()
            .map(|t| format!("{} {} GiB", t.name, t.gib))
            .collect::<Vec<_>>()
            .join(", "),
        planted.map_or_else(String::new, |p| format!(
            ", a tool cache of {} GiB at {} (its {ESCAPE} a link to {})",
            p.bytes >> 30,
            p.cache.display(),
            ballast.display()
        )),
        laid >> 20,
        g.floor() >> 20,
        if tabs.iter().any(|t| t.silent) {
            format!(
                "; silent: {}",
                tabs.iter()
                    .filter(|t| t.silent)
                    .map(|t| t.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            String::new()
        }
    );
    v.check(
        "the home is laid below its floor and past the emergency line",
        ((EMERGENCY_FREE_GIB * GIB)..g.floor()).contains(&laid),
        || format!("{} MiB free", laid >> 20),
    );
    Ok(())
}

/// Leave tab `t`'s draft ([`Tab::draft`]) unsent in its composer once its agent reads idle at its
/// turn's end: typed, never entered, and read back off the composer.
fn leave_draft(door: &Door<'_>, t: &Tab, v: &mut Verdict) -> Result<(), Fail> {
    let Some(draft) = t.draft.as_deref() else {
        return Ok(());
    };
    let at = format!("@{}", t.sid);
    // A hang detector of its turn's end.
    let idle = wait_for(HANG, || {
        kv(&door.out(&[&at, "status"]), "agent") == Some("idle")
    })?;
    if !idle || door.ctl(&[&at, "send", draft])?.code != 0 {
        return Err(could_not(&format!(
            "{}'s composer did not take its draft (idle: {idle}):\n{}",
            t.name,
            door.out(&[&at, "text"])
        )));
    }
    let composer = || crate::screen::composer_row("claude", &door.out(&[&at, "text"]));
    // A hang detector of the composer drawing what it was sent.
    let stands = wait_for(HANG, || composer().as_deref() == Some(draft))?;
    println!("  draft     {} @{}: {draft:?}", t.name, t.sid);
    v.check(
        &format!("{}'s draft stands in its composer, unsent", t.name),
        stands,
        || format!("its composer reads {:?}", composer()),
    );
    Ok(())
}

/// Run scenario `sc` on the lab `tag` that [`cmd_disk_ask`] brought up.
#[allow(clippy::too_many_lines)]
fn lane(tag: &str, sc: Scenario, v: &mut Verdict) -> Result<(), Fail> {
    let g = sc.geometry();
    let run = Run::load(tag)?;
    let door = Door::new(&run)?;
    let home = std::fs::canonicalize(run.w.join("home"))
        .map_err(|e| Fail::die(format!("the scratch home: {e}")))?;
    println!(
        "disk-ask: scenario {}, lab '{tag}', Claude Code {}{}, {}{}; HOME on its {} image at {}",
        sc.word(),
        run.claude_ver,
        if run.reroute == "1" {
            format!(" (its clone on the sessions' PATH, {})", run.claude_bin)
        } else {
            String::new()
        },
        run.need_aterm()?.display(),
        if run.jail_scan == "1" {
            " (its disk scans read the jail's processes)"
        } else {
            ""
        },
        g.image(),
        home.display()
    );
    let mut nonce = [0u8; 4];
    let _ = aterm_uds::rand::fill(&mut nonce);
    let nonce = format!("{:08x}", u32::from_ne_bytes(nonce));
    let mut tabs: Vec<Tab> = g
        .tabs
        .iter()
        .zip(g.silent)
        .map(|((name, gib), silent)| Tab {
            name,
            gib: *gib,
            token: format!("LABDISK-{name}-{nonce}"),
            dir: home.join("work").join(name),
            sid: String::new(),
            silent: *silent,
            draft: sc.drafted().then(|| draft_of(name)),
        })
        .collect();
    let planted = (g.cache_gib > 0).then(|| Planted {
        home: home.clone(),
        cache: home.join(CACHE),
        bytes: g.cache(),
        outside: std::iter::once(home.join(BALLAST))
            .chain(tabs.last().map(|t| t.dir.join("target")))
            .collect(),
    });
    for t in &tabs {
        lay_source(&t.dir)?;
    }
    // A drafted scenario lays the home short only once every draft stands, so the looks before
    // read the volume fine and rest, and the first that reads it short finds every draft.
    if !sc.drafted() {
        lay_short(&home, g, &tabs, planted.as_ref(), v)?;
    }

    crate::verbs::settings_merge(
        &run,
        &J::obj([(
            "permissions",
            J::obj([("allow", J::Arr(vec![J::s(format!("Bash({CLEANUP})"))]))]),
        )]),
    )?;
    crate::verbs::set_custom_mode(&run, MODE, &stub_mode(&tabs, planted.as_ref()))?;
    crate::forever::trust_for_claude(
        &run,
        &tabs.iter().map(|t| t.dir.clone()).collect::<Vec<_>>(),
    )?;
    crate::procs::need_witness(&run)?;
    let script = run.run.join("claude-disk-ask.sh");
    atomic_write(
        &script,
        crate::lab::launcher(&run, &Launch::default()).as_bytes(),
    )
    .map_err(|e| Fail::die(format!("cannot write {}: {e}", script.display())))?;
    let launch = format!("sh {}", crate::util::shell_quote(&script.to_string_lossy()));
    for t in &mut tabs {
        let r = door.ctl(&["spawn", &format!("cwd={}", t.dir.display()), "raise=false"])?;
        t.sid = r
            .stdout
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("OK "))
            .and_then(|l| l.split_whitespace().next())
            .map(str::to_owned)
            .ok_or_else(|| {
                could_not(&format!(
                    "spawn cwd={} said {:?}{}",
                    t.dir.display(),
                    r.stdout.trim(),
                    r.stderr.trim()
                ))
            })?;
        let at = format!("@{}", t.sid);
        door.ctl(&[&at, "await", "idle", "400", "timeout=30000"])?;
        door.ctl(&[&at, "send", &launch])?;
        door.ctl(&[&at, "key", "enter"])?;
    }
    for t in &tabs {
        let at = format!("@{}", t.sid);
        if let Some(why) =
            crate::forever::empty_composer(&door, &t.sid, "claude")?.why("Claude Code")
        {
            return Err(could_not(&format!(
                "{}: {why}:\n{}",
                t.name,
                door.out(&[&at, "text"])
            )));
        }
        let task = format!(
            "{}: keep this checkout building; I will check back later.",
            t.token
        );
        if !crate::forever::tab_turn(&door, &t.sid, "claude", &task)? {
            return Err(could_not(&format!(
                "{}'s composer did not take its task:\n{}",
                t.name,
                door.out(&[&at, "text"])
            )));
        }
    }
    for t in &tabs {
        let answered = tab_shows(&door, &t.sid, "Working on it", Duration::from_secs(90))?;
        let status = door.out(&[&format!("@{}", t.sid), "status"]);
        let cwd = door.out(&[&format!("@{}", t.sid), "cwd"]);
        println!(
            "  tab       {} @{} in {}: answered={answered} supervisor={} autopilot={} agent={} disk={} cwd={}",
            t.name,
            t.sid,
            t.dir.display(),
            kv(&status, "supervisor").unwrap_or("-"),
            kv(&status, "autopilot").unwrap_or("-"),
            kv(&status, "agent").unwrap_or("-"),
            kv(&status, "disk").unwrap_or("-"),
            cwd.trim()
        );
        v.check(
            &format!("{} answered its task and is supervised", t.name),
            answered
                && kv(&status, "supervisor").is_some_and(|s| s.starts_with("aterm-harness"))
                && kv(&status, "autopilot").is_some_and(|a| a != "off" && a != "-"),
            || status.trim().chars().take(400).collect(),
        );
    }
    if sc.drafted() {
        for t in &tabs {
            leave_draft(&door, t, v)?;
        }
        lay_short(&home, g, &tabs, planted.as_ref(), v)?;
    }
    if planted.is_some() {
        // No tab ran a command, so no shell snapshot stands: the one the worker's fence proves
        // its commands against is the worker's own to write.
        let snapshots = home.join(".claude/shell-snapshots");
        let shots: Vec<String> = std::fs::read_dir(&snapshots)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("snapshot-") && n.ends_with(".sh"))
            .collect();
        println!("  snapshot  {shots:?} in {}", snapshots.display());
        v.check(
            "no Claude Code shell snapshot stands before the worker starts (no tab ran a command): the snapshot its fence proves commands against is its own to write",
            shots.is_empty(),
            || format!("{shots:?} in {}", snapshots.display()),
        );
    }
    let lane = Lane {
        run: &run,
        door: &door,
        ledger: ledger(&run),
        home,
        tabs,
        floor: g.floor(),
        laid_free: g.laid_free,
        planted,
        t0: Instant::now(),
        printed: Cell::new(0),
    };
    println!("  tabs up   {}", lane.since(lane.t0));
    if sc == Scenario::Worker {
        return dedicated(&lane, v);
    }
    let Some(h) = head(&lane, v)? else {
        return Ok(());
    };
    match sc {
        Scenario::Clear => clear(&lane, v, &h),
        Scenario::Partial => partial(&lane, v, &h),
        Scenario::Unanswered => unanswered(&lane, v, &h),
        Scenario::Exhausted => exhausted(&lane, v, &h),
        // Run above: it has no line, so no head.
        Scenario::Worker => Ok(()),
    }
}

/// The lane's lab, up: taken down when it drops ([`crate::lab::down`]), so a lane that panics or
/// returns early still ends its instance and detaches its home image, which holds the builds and
/// ballast it laid (up to its image's size of the host's space).
struct Lab {
    tag: String,
    /// Still to be taken down (not `--keep`, nor taken down already).
    up: bool,
}

impl Lab {
    /// Take the lab down now: where its logs were kept.
    /// # Errors
    /// It did not come down.
    fn down(mut self) -> Result<PathBuf, Fail> {
        self.up = false;
        crate::lab::down(&self.tag, false)
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        if !self.up {
            return;
        }
        match crate::lab::down(&self.tag, false) {
            Ok(k) => eprintln!(
                "aterm-lab: disk-ask: lab '{}' taken down on the lane's way out; logs kept in {}",
                self.tag,
                k.display()
            ),
            Err(f) => eprintln!(
                "aterm-lab: disk-ask: lab '{}' did not come down ({}): {LAB} down {} takes it down",
                self.tag,
                f.msg.unwrap_or_default(),
                self.tag
            ),
        }
    }
}

/// `disk-ask [tag] [--keep] [--scenario clear|partial|unanswered|exhausted|worker]`.
/// # Errors
/// A usage error, an interrupt, or a lab that could not be read.
pub fn cmd_disk_ask(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("disk-ask: {m}"));
    let names = Scenario::ALL.map(Scenario::word).join("|");
    let (mut tag, mut keep, mut sc) = (None::<String>, false, Scenario::Clear);
    let mut args = args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--keep" => keep = true,
            "--scenario" => {
                let word = args
                    .next()
                    .ok_or_else(|| usage(&format!("--scenario takes one of {names}")))?;
                sc = Scenario::named(word)
                    .ok_or_else(|| usage(&format!("unknown scenario '{word}' (one of {names})")))?;
            }
            s if s.starts_with('-') => {
                return Err(usage(&format!(
                    "unknown option '{s}' (--keep, --scenario {names})"
                )));
            }
            s => tag = Some(s.to_string()),
        }
    }
    let g = sc.geometry();
    let tag = tag.unwrap_or_else(|| format!("disk-ask-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: disk-ask brings up a lab of its own (another tag, or {LAB} down {tag})"
        )));
    }
    if !Path::new("/usr/bin/hdiutil").exists() {
        println!("disk-ask: NOT RUN (skip: no hdiutil on this host)");
        return Ok(3);
    }
    let need = crate::lab::home_image_bytes(&g.image()).map_err(Fail::die)? + HOST_SPARE;
    match free_on(&crate::paths::paths().state) {
        Some(f) if f >= need => {}
        free => {
            println!(
                "disk-ask: NOT RUN (skip: {} free here, the {} scenario's {} image needs {} GiB so it cannot be what fills the disk)",
                free.map_or_else(|| "unknown".to_owned(), |f| format!("{} GiB", f >> 30)),
                sc.word(),
                g.image(),
                need >> 30
            );
            return Ok(3);
        }
    }
    stop::install();
    let mut u = UpOpts::from_env();
    u.lifeline = !keep;
    // aterm starts its disk worker as plain `claude`, which the session's `PATH` finds through the
    // reroute, as an install's managed Claude Code: without it the worker never starts.
    u.reroute |= sc.claims_worker();
    // A worker that frees a cache: `aterm harness disk free` rules out no path while a process
    // stands unread, and from inside the jail the owner's processes cannot be read, so the
    // instance's disk scans read the jail's alone ([`crate::lab::UpOpts::jail_scan`]).
    u.jail_scan |= g.cache_gib > 0;
    if sc.drafted() {
        // The owner's grace after a person's keystroke ([`DRAFT_GRACE`]): without it a tab's
        // Cruise sends the draft as its next continuation, two minutes on, and no draft holds.
        u.harness_keys = format!("{}\nhuman_grace_s = {DRAFT_GRACE}\n", u.harness_keys);
    }
    u.home_image = Some(g.image());
    u.aterm_toml = format!("{}\n{}", u.aterm_toml, g.disk_toml());
    u.instance_env = format!(
        "{} {}=1",
        u.instance_env,
        aterm_agent::harness::disk::WATCH_SEAM
    )
    .trim()
    .to_owned();
    if let Err(f) = crate::lab::up(&tag, &u) {
        if let Some(m) = &f.msg {
            eprintln!("aterm-lab: {m}");
        }
        if f.stopped.is_some() {
            return Err(f);
        }
        eprintln!("aterm-lab: disk-ask: COULD NOT RUN: the lab did not come up");
        return Ok(3);
    }
    // Taken down on every way out of the lane, a panic's among them, unless `--keep` left it up.
    let lab = Lab {
        tag: tag.clone(),
        up: !keep,
    };
    let mut v = Verdict::default();
    let ran = lane(&tag, sc, &mut v);
    if keep {
        println!("disk-ask: lab '{tag}' left up, its home image attached ({LAB} down {tag})");
    } else {
        match lab.down() {
            Ok(k) => println!("lab '{tag}' is down; logs kept in {}", k.display()),
            Err(f) => v.check("the lab came down, its home image detached", false, || {
                format!(
                    "{} ({LAB} down {tag} takes it down)",
                    f.msg.unwrap_or_default()
                )
            }),
        }
    }
    match ran {
        Err(f) if f.stopped.is_some() => return Err(f),
        Err(f) if f.code == 3 => {
            if let Some(m) = &f.msg {
                println!("{m}");
            }
            return Ok(3);
        }
        Err(f) => v.check("the lane ran to its end", false, || {
            f.msg.unwrap_or_default()
        }),
        Ok(()) => {}
    }
    if v.passed() {
        println!("disk-ask: {} PASS ({} checks)", sc.word(), v.rows.len());
        Ok(0)
    } else {
        println!("disk-ask: {} FAIL", sc.word());
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ledgers_rows_are_read_by_their_rule() {
        let text = concat!(
            "{\"kind\":\"removed\",\"tier\":4}\n",
            "{\"ts\":\"t1\",\"kind\":\"free-ask\",\"said\":\"elected\",\"sid\":\"s-1\",\"free_bytes\":5}\n",
            "{\"ts\":\"t2\",\"kind\":\"free-ask\",\"said\":\"step\",\"sid\":\"s-1\",\"step\":\"typed\"}\n",
            "{\"ts\":\"t3\",\"kind\":\"free-ask\",\"said\":\"handoff\",\"sid\":\"s-1\",\"why\":\"nothing-freed\"}\n",
        );
        let rows = ask_rows(text);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            (rows[0].said.as_str(), rows[0].sid.as_str(), rows[0].free),
            ("elected", "s-1", Some(5))
        );
        assert_eq!(
            (rows[1].said.as_str(), rows[1].step.as_str()),
            ("step", "typed")
        );
        assert_eq!(
            (rows[2].said.as_str(), rows[2].why.as_str()),
            ("handoff", Miss::NothingFreed.word())
        );
        assert_eq!(count(&rows, "step", "typed"), 1);
        assert_eq!(count(&rows, "handoff", ""), 1);
        assert_eq!(count(&rows, "step", ""), 0);
    }

    fn tab(name: &'static str) -> Tab {
        Tab {
            name,
            gib: 1,
            token: format!("LABDISK-{name}-1"),
            dir: PathBuf::from(format!("/w/{name}")),
            sid: String::new(),
            silent: false,
            draft: None,
        }
    }

    fn user(text: &str) -> crate::workers::Row {
        crate::workers::Row {
            kind: "user".to_owned(),
            text: text.to_owned(),
            ..Default::default()
        }
    }

    /// What each tab received is read off every transcript: a line in a conversation no tab's
    /// token names counts, with its file, and a tab with no transcript is unread, never a tab
    /// that received nothing. Before this a line typed into a conversation the lane could not
    /// match to its tab (a new session file, a first row that is not the task) went uncounted,
    /// and "exactly one tab received the line" held although two had.
    #[test]
    fn every_transcripts_lines_are_counted_and_an_unmatched_tab_is_unread() {
        let tabs = [tab("alpha"), tab("beta"), tab("gamma")];
        let line = "[aterm harness] aterm: ... Only this tab was asked. (rule disk-free@v1)";
        let result = crate::workers::Row {
            kind: "user".to_owned(),
            text: "removed (rule disk-free@v1)".to_owned(),
            result: Some("removed (rule disk-free@v1)".to_owned()),
            ..Default::default()
        };
        let all = vec![
            (
                PathBuf::from("/p/a.jsonl"),
                vec![
                    user("LABDISK-alpha-1: keep this checkout building"),
                    user("continue"),
                    user(line),
                    result,
                ],
            ),
            (
                PathBuf::from("/p/b.jsonl"),
                vec![user("LABDISK-beta-1: keep this checkout building")],
            ),
        ];
        let got = received_in(&all, &tabs);
        assert_eq!(got.total(), 1, "a tool's result is no line: {got:?}");
        assert_eq!(got.counts(), [1, 0, 0]);
        assert_eq!(got.unread(), ["gamma"], "no transcript of gamma's");
        assert_eq!(got.first(), Some(line));
        assert!(
            !got.exactly(&[1, 0, 0]),
            "an unread tab is not one that received nothing"
        );
        let mut all = all;
        all.push((
            PathBuf::from("/p/g.jsonl"),
            vec![user("LABDISK-gamma-1: keep this checkout building")],
        ));
        let got = received_in(&all, &tabs);
        assert!(got.exactly(&[1, 0, 0]), "{got:?}");
        assert_eq!(got.first_of(0), Some(line));
        assert_eq!(got.first_of(1), None);
        all.push((
            PathBuf::from("/p/c.jsonl"),
            vec![user("resumed"), user(line)],
        ));
        let got = received_in(&all, &tabs);
        assert_eq!(got.total(), 2, "{got:?}");
        assert_eq!(got.counts(), [1, 0, 0]);
        assert_eq!(got.stray, [(PathBuf::from("/p/c.jsonl"), line.to_owned())]);
        assert!(!got.exactly(&[1, 0, 0]), "a stray line fails: {got:?}");
        // A tab's second transcript counts for it.
        all.push((
            PathBuf::from("/p/a2.jsonl"),
            vec![user("LABDISK-alpha-1: again"), user(line)],
        ));
        let got = received_in(&all, &tabs);
        assert_eq!(got.tabs[0].1.as_ref().map(Vec::len), Some(2), "{got:?}");
        assert_eq!(got.counts(), [2, 0, 0]);
    }

    /// aterm's disk worker is no tab: its task ends `(rule disk-free@v1 worker)`, which neither
    /// counts as a line nor draws the stub's cleanup. Before this the lane matched the rule's bare
    /// name, so a worker that started would have read as a line in a conversation of no tab's.
    #[test]
    fn the_disk_workers_task_is_no_line() {
        let tabs = [tab("alpha"), tab("beta"), tab("gamma")];
        let task = "Free this volume ... no NEEDS YOU line: aterm tells the owner. (rule disk-free@v1 worker)";
        let all = vec![
            (
                PathBuf::from("/p/a.jsonl"),
                vec![user("LABDISK-alpha-1: keep this checkout building")],
            ),
            (
                PathBuf::from("/p/b.jsonl"),
                vec![user("LABDISK-beta-1: keep this checkout building")],
            ),
            (
                PathBuf::from("/p/g.jsonl"),
                vec![user("LABDISK-gamma-1: keep this checkout building")],
            ),
            (PathBuf::from("/p/w.jsonl"), vec![user(task)]),
        ];
        let got = received_in(&all, &tabs);
        assert!(
            got.exactly(&[0, 0, 0]),
            "the worker's own conversation: {got:?}"
        );
        // The worker's task typed into a tab's conversation is that tab asked again, though it is
        // no line: before this the lane matched only the line's tag and passed it.
        let mut spammed = all.clone();
        spammed[0].1.push(user(task));
        let got = received_in(&spammed, &tabs);
        assert_eq!(got.counts(), [0, 0, 0], "no line: {got:?}");
        assert_eq!(
            got.worker_in_tab,
            [("alpha".to_owned(), PathBuf::from("/p/a.jsonl"))]
        );
        assert!(!got.exactly(&[0, 0, 0]), "{got:?}");
        let picked = crate::stub::with_reply(stub_mode(&tabs, None), &users(&[task]));
        assert_eq!(picked.get("text").and_then(J::as_str), Some(WORKING));
        assert!(crate::stub::scripted_call(&picked, false).is_none());
        let rows = ask_rows(concat!(
            "{\"ts\":\"t1\",\"kind\":\"free-ask\",\"said\":\"worker\",\"sid\":\"\"}\n",
            "{\"ts\":\"t2\",\"kind\":\"free-ask\",\"said\":\"worker-started\",\"sid\":\"s-w\"}\n",
            "{\"ts\":\"t3\",\"kind\":\"free-ask\",\"said\":\"worker-over\",\"sid\":\"s-w\",\"why\":\"stalled:startup\"}\n",
        ));
        assert_eq!(count(&rows, "worker", ""), 1, "only the claim");
        assert_eq!(
            (rows[2].said.as_str(), rows[2].why.as_str()),
            ("worker-over", "stalled:startup")
        );
        assert_eq!(worker_sids(&rows, 0), ["s-w"]);
        assert!(worker_sids(&rows, 3).is_empty());
    }

    /// The disk worker runs in a session of its own: one the ledger's worker rows name that is a
    /// tab's is found, whichever worker row names it.
    #[test]
    fn a_worker_in_a_tabs_session_is_found() {
        let mut tabs = [tab("alpha"), tab("beta"), tab("gamma")];
        for (t, sid) in tabs.iter_mut().zip(["s-a", "s-b", "s-g"]) {
            t.sid = sid.to_owned();
        }
        let own = ask_rows(concat!(
            "{\"kind\":\"free-ask\",\"said\":\"elected\",\"sid\":\"s-a\"}\n",
            "{\"kind\":\"free-ask\",\"said\":\"worker\",\"sid\":\"\"}\n",
            "{\"kind\":\"free-ask\",\"said\":\"worker-started\",\"sid\":\"s-w\"}\n",
            "{\"kind\":\"free-ask\",\"said\":\"worker-runs\",\"sid\":\"s-w\"}\n",
            "{\"kind\":\"free-ask\",\"said\":\"worker-over\",\"sid\":\"s-w\"}\n",
        ));
        assert_eq!(worker_sids(&own, 0), ["s-w"], "an elected tab is no worker");
        assert!(tabs_among(&worker_sids(&own, 0), &tabs).is_empty());
        for said in ["worker-started", "worker-runs", "worker-over"] {
            let rows = ask_rows(&format!(
                "{{\"kind\":\"free-ask\",\"said\":\"{said}\",\"sid\":\"s-b\"}}\n"
            ));
            assert_eq!(
                tabs_among(&worker_sids(&rows, 0), &tabs),
                [("beta", "s-b")],
                "{said}"
            );
        }
    }

    #[test]
    fn the_asked_tabs_own_build_is_read_by_its_mark() {
        let own = "Largest left: ~/work/alpha's builds 7.0 GiB (this tab's). Free what";
        assert!(marks_own(own, "alpha"));
        assert!(!marks_own(own, "beta"));
        let other = "Largest left: ~/work/alpha's builds 7.0 GiB; ~/work/beta's builds 2.0 GiB (this tab's).";
        assert!(!marks_own(other, "alpha"), "the mark is beta's");
        assert!(marks_own(other, "beta"));
        let shared = "Largest left: ~/work/alpha's builds 7.0 GiB (this tab's checkout, shared).";
        assert!(!marks_own(shared, "alpha"), "shared is not its own alone");
        let pointer = "`aterm harness disk` lists the largest holders left.";
        assert!(!marks_own(pointer, "alpha"));
    }

    fn users(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_stub_answers_only_the_disk_line_with_the_cleanup() {
        let spec = stub_mode(&[tab("alpha"), tab("beta"), tab("gamma")], None);
        let task = users(&["LABDISK-alpha-1: keep this checkout building"]);
        let picked = crate::stub::with_reply(spec.clone(), &task);
        assert_eq!(picked.get("text").and_then(J::as_str), Some(WORKING));
        assert!(crate::stub::scripted_call(&picked, false).is_none());
        let asked = users(&[
            "LABDISK-alpha-1: keep this checkout building",
            "continue",
            "[aterm] aterm: ... Only this tab was asked. (rule disk-free@v1)",
        ]);
        let picked = crate::stub::with_reply(spec.clone(), &asked);
        let (name, input) = crate::stub::scripted_call(&picked, false).expect("the call");
        assert_eq!(name, "Bash");
        assert_eq!(input.get("command").and_then(J::as_str), Some(CLEANUP));
        assert!(
            crate::stub::scripted_call(&picked, true).is_none(),
            "its result back"
        );
        assert_eq!(picked.get("text").and_then(J::as_str), Some(REMOVED));
        let after = users(&[
            "LABDISK-alpha-1: keep this checkout building",
            "[aterm] aterm: ... (rule disk-free@v1)",
            "continue",
        ]);
        let picked = crate::stub::with_reply(spec, &after);
        assert_eq!(picked.get("text").and_then(J::as_str), Some(WORKING));
    }

    /// A silent tab, told apart by its token in its conversation's prompts, answers the disk line
    /// with words alone; every other tab answers it with the cleanup.
    #[test]
    fn a_silent_tab_answers_the_disk_line_with_words_alone() {
        let mut alpha = tab("alpha");
        alpha.silent = true;
        let spec = stub_mode(&[alpha, tab("beta"), tab("gamma")], None);
        let line = "[aterm] aterm: ... Only this tab was asked. (rule disk-free@v1)";
        let silent = users(&["LABDISK-alpha-1: keep this checkout building", line]);
        let picked = crate::stub::with_reply(spec.clone(), &silent);
        assert_eq!(picked.get("text").and_then(J::as_str), Some(DECLINED));
        assert!(crate::stub::scripted_call(&picked, false).is_none());
        let asked = users(&["LABDISK-beta-1: keep this checkout building", line]);
        let picked = crate::stub::with_reply(spec.clone(), &asked);
        let (_, input) = crate::stub::scripted_call(&picked, false).expect("beta's call");
        assert_eq!(input.get("command").and_then(J::as_str), Some(CLEANUP));
        let task = users(&["LABDISK-alpha-1: keep this checkout building", "continue"]);
        let picked = crate::stub::with_reply(spec, &task);
        assert_eq!(picked.get("text").and_then(J::as_str), Some(WORKING));
    }

    #[test]
    fn a_scenario_is_named_by_its_word() {
        for sc in Scenario::ALL {
            assert_eq!(Scenario::named(sc.word()), Some(sc));
        }
        assert_eq!(Scenario::named("ended"), None);
        assert_eq!(
            UNANSWERED.disk_toml(),
            "[disk]\nwarn_free_gib = 9\nauto_free_gib = 8\nidle_build_dirs = true\n"
        );
        assert_eq!(PARTIAL.image(), "10g");
        assert_eq!(EXHAUSTED.disk_toml(), UNANSWERED.disk_toml());
        assert_eq!(silent(&EXHAUSTED), 2);
        assert_eq!(silent(&UNANSWERED), 1);
        assert_eq!(Scenario::named("worker"), Some(Scenario::Worker));
        assert_eq!(WORKER.image(), "11g");
        assert_eq!(WORKER.tabs.len(), 2);
        assert!(Scenario::Worker.drafted() && !Scenario::Clear.drafted());
        assert!(!Scenario::Partial.claims_worker());
        for sc in [
            Scenario::Clear,
            Scenario::Unanswered,
            Scenario::Exhausted,
            Scenario::Worker,
        ] {
            assert!(sc.claims_worker(), "{sc:?}");
        }
    }

    /// Each scenario's `[disk]` makes the lines the lane's sizes stand on, as the window computes
    /// them on the image: the floor written, the emergency line 4 GiB, the home laid `short`.
    #[test]
    fn each_scenarios_disk_makes_the_lines_its_sizes_stand_on() {
        for sc in Scenario::ALL {
            let g = sc.geometry();
            let knobs = aterm_capacity::floor::Knobs::from_toml(&g.disk_toml());
            let lines = aterm_capacity::floor::lines(knobs, Some(g.image_gib * GIB), None);
            assert_eq!(lines.floor, g.floor(), "{sc:?}");
            assert_eq!(lines.emergency, EMERGENCY_FREE_GIB * GIB, "{sc:?}");
            let t = aterm_agent::harness::disk::Thresholds {
                warn: lines.warn,
                floor: lines.floor,
                target: lines.target,
                emergency: lines.emergency,
                files_emergency: lines.files_emergency,
            };
            assert_eq!(
                t.level(Some(g.laid_free), None),
                aterm_agent::harness::disk::Level::Short,
                "{sc:?}"
            );
            assert_eq!(
                t.level(Some(g.laid_free + g.cache()), None),
                aterm_agent::harness::disk::Level::Short,
                "{sc:?}: the cache freed leaves the volume short"
            );
        }
    }

    /// A request body of `msgs` (each a message's JSON).
    fn body(msgs: &[String]) -> Vec<u8> {
        format!("{{\"messages\":[{}]}}", msgs.join(",")).into_bytes()
    }

    fn user_msg(text: &str) -> String {
        msg("user", text)
    }

    fn reply_msg(text: &str) -> String {
        msg("assistant", text)
    }

    fn msg(role: &str, text: &str) -> String {
        J::obj([("role", J::s(role)), ("content", J::s(text))]).dump(crate::pyjson::Style {
            indent: None,
            compact: true,
            ascii: false,
        })
    }

    /// A planted cache as the lane lays it under home `/h`, its outside paths the ballast and
    /// beta's build.
    fn planted() -> Planted {
        Planted {
            home: PathBuf::from("/h"),
            cache: PathBuf::from("/h/Library/Caches/labtool"),
            bytes: 2 * GIB,
            outside: vec![
                PathBuf::from("/h/ballast"),
                PathBuf::from("/h/work/beta/target"),
            ],
        }
    }

    /// The commands the stub drives aterm's disk worker through under `script`, each answered
    /// with a result, and the report it then says.
    fn driven(script: &J, task: &str) -> (Vec<String>, String, Vec<String>) {
        use crate::stub::{ToolTurn, keyed_tool_turn};
        let used = r#"{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}"#;
        let result = r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}"#;
        let mut msgs = vec![user_msg(task)];
        let mut commands = Vec::new();
        loop {
            match keyed_tool_turn(&body(&msgs), script) {
                Some(ToolTurn::Call(name, input, step)) => {
                    assert_eq!((name.as_str(), step), ("Bash", commands.len()));
                    commands.push(
                        input
                            .get("command")
                            .and_then(J::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    );
                    msgs.extend([used.to_owned(), result.to_owned()]);
                }
                Some(ToolTurn::Say(text, _)) => return (commands, text, msgs),
                other => panic!("{other:?} after {commands:?}"),
            }
        }
    }

    /// aterm's disk worker is driven by its task's tag alone, and through its fence's steps. With a
    /// cache planted, as a worker that skipped its task's first step: the dry run before any
    /// command (which its fence must decline, no shell snapshot standing), `pwd`, the `rm -rf` of
    /// the cache (which its fence must decline), the dry run again, the verb of the paths it must
    /// refuse by their whole paths (which its fence must decline, drawn on two rows), then under
    /// `~`, the verb's removal of the cache, then its report, said again to Autopilot Land's
    /// prompt as its defense. With none, as its task asks: `pwd`, the dry run and its report. A
    /// tab's conversation, the disk line in it or not, takes no step of it: it is the replies'
    /// (the line's cleanup), and a tab runs no command but that.
    #[test]
    fn the_stub_drives_the_disk_worker_through_its_steps_and_no_tab() {
        use crate::stub::{ToolTurn, keyed_tool_turn};
        let tabs = [tab("alpha"), tab("beta")];
        let p = planted();
        let task = format!("Free this volume. {}", worker_tag());
        let land = user_msg(
            "[aterm harness] Autopilot is Land: ... defend that in a few lines starting Nothing left:",
        );
        let spec = stub_mode(&tabs, Some(&p));
        let script = spec.get("script").cloned().expect("the script");
        let (commands, report, mut msgs) = driven(&script, &task);
        assert_eq!(
            commands,
            [
                FREE_DRY,
                FIRST_BASH,
                "rm -rf /h/Library/Caches/labtool",
                FREE_DRY,
                "aterm harness disk free /h/ballast /h/work/beta/target",
                "aterm harness disk free ~/ballast ~/work/beta/target",
                "aterm harness disk free /h/Library/Caches/labtool",
            ]
        );
        assert_eq!(commands[2], rm_cache(&p.cache));
        assert_eq!(commands[5], free_in_home(&p));
        assert!(is_report(&report), "{report}");
        assert!(
            report.contains("/h/Library/Caches/labtool (2.0 GiB)"),
            "{report}"
        );
        msgs.extend([reply_msg(&report), land.clone()]);
        let defense = keyed_tool_turn(&body(&msgs), &script);
        assert!(
            matches!(&defense, Some(ToolTurn::Say(t, 8)) if *t == report),
            "Land's prompt is answered with the report: {defense:?}"
        );
        msgs.extend([reply_msg(&report), land]);
        assert_eq!(keyed_tool_turn(&body(&msgs), &script), Some(ToolTurn::Done));
        let line = user_msg("[aterm harness] ... Only this tab was asked. (rule disk-free@v1)");
        for first in [
            "LABDISK-alpha-1: keep this checkout building",
            "LABDISK-beta-1: keep this checkout building",
        ] {
            assert_eq!(
                keyed_tool_turn(&body(&[user_msg(first)]), &script),
                None,
                "a tab's task is the replies'"
            );
            assert_eq!(
                keyed_tool_turn(&body(&[user_msg(first), line.clone()]), &script),
                None,
                "a tab's line is the replies'"
            );
        }
        let picked = crate::stub::with_reply(
            spec,
            &users(&["LABDISK-alpha-1: keep", "x (rule disk-free@v1)"]),
        );
        let (_, input) = crate::stub::scripted_call(&picked, false).expect("the tab's cleanup");
        assert_eq!(input.get("command").and_then(J::as_str), Some(CLEANUP));
        // No cache: `pwd`, the dry run, then the report, which freed nothing.
        let (commands, report, _) = driven(&worker_script(None), &worker_tag());
        assert_eq!(commands, [FIRST_BASH, FREE_DRY]);
        assert!(
            is_report(&report) && report.contains("freed nothing"),
            "{report}"
        );
    }

    /// An approval-ledger row is read as the box of the command its subject names, alone or after
    /// the box's kind, a row the box wrapped included; a command that only ends in its words is
    /// not it.
    #[test]
    fn a_ledger_row_is_read_as_the_box_of_its_command() {
        let row = |command: &str| Decided {
            command: command.to_owned(),
            ..Default::default()
        };
        assert!(boxed(&row("bash box: rm -rf /h/c"), "rm -rf /h/c"));
        assert!(boxed(&row(FIRST_BASH), FIRST_BASH));
        assert!(
            boxed(
                &row("aterm harness disk free /h/ballast /h/work/beta/tar get"),
                "aterm harness disk free /h/ballast /h/work/beta/target"
            ),
            "a command the box wrapped"
        );
        assert!(!boxed(&row("echo pwd"), FIRST_BASH));
        assert!(!boxed(
            &row("bash box: aterm harness disk free --dry-run"),
            FIRST_BASH
        ));
        assert!(!boxed(
            &row("aterm harness disk free /h/Library/Caches/labtool"),
            FREE_DRY
        ));
    }

    /// The worker's report is a defense Autopilot Land lands: a `Nothing left:` reply with no
    /// NEEDS YOU line and nothing Land reads as open work, freed a cache or nothing. CONTROL: a
    /// report that leaves work for later is none.
    #[test]
    fn the_workers_report_is_a_defense_land_lands() {
        use aterm_agent::supervise::policy::turn_end::{clean_defense, land_open_found};
        for report in [worker_report(Some(&planted())), worker_report(None)] {
            assert!(
                clean_defense(Some(&report)),
                "{report}: {:?}",
                land_open_found(Some(&report))
            );
            assert!(land_open_found(Some(&report)).is_empty(), "{report}");
        }
        let open = format!("{NOTHING_LEFT} freed nothing yet; I will free the cache later.");
        assert!(!clean_defense(Some(&open)), "{open}");
    }

    /// A worker's report is its last reply with words, read with whatever was typed into it
    /// after; a reply that lacks a line its task asks for is no report.
    #[test]
    fn a_workers_report_is_its_last_reply_and_what_follows_it_is_read() {
        let reply = |text: &str| crate::workers::Row {
            kind: "assistant".to_owned(),
            text: text.to_owned(),
            ts: Some(5),
            ..Default::default()
        };
        let call = crate::workers::Row {
            kind: "assistant".to_owned(),
            tool_use: Some(("Bash".to_owned(), J::obj([("command", J::s(FREE_DRY))]))),
            tool_id: "t1".to_owned(),
            ..Default::default()
        };
        let result = crate::workers::Row {
            kind: "user".to_owned(),
            result: Some("would remove /h/c (2.0 GiB)".to_owned()),
            tool_id: "t1".to_owned(),
            ..Default::default()
        };
        let said = worker_report(Some(&planted()));
        let rows = vec![user("the task"), call, result, reply(&said)];
        let (report, after) = report_of(&rows);
        assert_eq!(report.map(|r| r.text.as_str()), Some(said.as_str()));
        assert!(after.is_empty());
        assert_eq!(results_of(&rows, FREE_DRY), ["would remove /h/c (2.0 GiB)"]);
        assert!(results_of(&rows, "rm -rf /h/c").is_empty());
        let mut typed = rows.clone();
        typed.push(user("[aterm harness] Autopilot is Land: ..."));
        let (report, after) = report_of(&typed);
        assert!(report.is_some());
        assert_eq!(after.len(), 1, "the prompt typed after the report");
        assert!(is_report(&said));
        assert!(
            !is_report("Nothing left: freed it.\ndisk-free filling: ballast"),
            "no owner-only line"
        );
        assert!(
            !is_report("I freed it.\ndisk-free filling: a\ndisk-free owner-only: b"),
            "no Nothing left:"
        );
    }

    /// The ledger's `disk-free` rows and a session's approval ledger are read by their own fields.
    #[test]
    fn the_verbs_rows_and_the_approval_ledger_are_read() {
        let ledger = concat!(
            "{\"ts\":\"t1\",\"kind\":\"free-ask\",\"said\":\"worker-over\",\"sid\":\"s-w\",\"why\":\"finished\",\"rose_bytes\":2147483648}\n",
            "{\"ts\":\"t2\",\"sid\":\"s-w\",\"kind\":\"disk-free\",\"action\":\"removing\",\"path\":\"/h/c\",\"bytes\":2147483648,\"text\":\"a cache\"}\n",
            "{\"ts\":\"t3\",\"sid\":\"s-w\",\"kind\":\"disk-free\",\"action\":\"removed\",\"path\":\"/h/c\",\"bytes\":2147483648,\"text\":\"a cache\"}\n",
        );
        let rows = free_rows(ledger);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (
                rows[0].action.as_str(),
                rows[0].sid.as_str(),
                rows[0].path.as_str(),
                rows[0].bytes
            ),
            ("removing", "s-w", "/h/c", Some(2 * GIB))
        );
        assert_eq!(rows[1].action, "removed");
        let asked = ask_rows(ledger);
        assert_eq!(asked.len(), 1, "a disk-free row is no free-ask row");
        assert_eq!(asked[0].rose, Some(2 * GIB));
        let drive = concat!(
            "{\"ts\":10,\"sid\":\"s-w\",\"rule_id\":\"disk-worker@v1\",\"decision\":\"declined\",\"command\":\"rm -rf /h/c\",\"reason\":\"r\"}\n",
            "{\"ts\":20,\"sid\":\"s-w\",\"rule_id\":\"continue@v1\",\"decision\":\"typed\",\"command\":\"[aterm harness] go on\"}\n",
        );
        let d = decisions(drive);
        assert_eq!(
            d[0],
            Decided {
                ts: 10,
                rule: "disk-worker@v1".to_owned(),
                decision: "declined".to_owned(),
                command: "rm -rf /h/c".to_owned(),
                reason: "r".to_owned(),
            }
        );
        assert_eq!((d[1].ts, d[1].decision.as_str()), (20, "typed"));
        assert_eq!(squashed("a b\n c"), "abc");
    }
}
