// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `disk-ask [tag] [--keep] [--scenario clear|partial|unanswered|exhausted]`: the ask to free
//! disk space (`disk-free@v1`, `docs/DESIGN-disk-free-ask-2026-10-24.md`) end to end, on a real
//! aterm with real Claude Code.
//!
//! A jailed lab of its own ([`crate::lab::up`]) whose `HOME` is a sparse APFS image
//! (`LAB_HOME_IMAGE`), its window the owner's disk watch there under the watch's development seam
//! ([`aterm_agent::harness::disk::WATCH_SEAM`]). The home holds one checkout per tab, each with a
//! recent build that aterm's own pass leaves (its bytes in `deps/`), and ballast that is no build,
//! so the volume stays below its floor once aterm's pass has done what it can. Three Claude Code
//! tabs, each in its own checkout and given a task, rest under the harness. The stub answers the
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
//!
//! The window starts the worker with plain `claude`, which the lab's `PATH` does not hold (its tabs
//! run Claude Code through the lab's launcher), so in a lab it ends `stalled:startup` about two
//! minutes after its claim; the lane waits on any end, a hang detector of its longest life
//! ([`ask::SPAWN_MS`] and [`ask::WORKER_MS`]). Wherever it runs, it runs in a session of its own:
//! a worker row naming a tab's session, or its task (`(rule disk-free@v1 worker)`) in a tab's
//! conversation, fails the lane, as that is the owner's tab asked a second time.

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

/// What a home image holds past the builds and the free space the lane lays: the file system's
/// own, and what three Claude Code sessions write in their homes. The rest is ballast.
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
    tabs: [(&'static str, u64); 3],
    /// Which tabs' agents answer the disk line with words alone, removing nothing.
    silent: [bool; 3],
}

impl Geometry {
    const fn floor(&self) -> u64 {
        self.floor_gib * GIB
    }

    const fn tab(&self, i: usize) -> u64 {
        self.tabs[i].1 * GIB
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

/// What every scenario's sizes hold: the first tab's build is the largest by whole GiB, so the
/// election picks it; the floor and the warning sit above the emergency line, which is 4 GiB on
/// the image (its 1% is less); the home is laid below its floor and past the emergency line; and
/// the image holds the builds, the free space laid and [`HEADROOM`], in at most
/// [`MAX_IMAGE_GIB`], and a GiB less would not hold them, so the image is no larger than the
/// sizes need.
const fn sound(g: &Geometry) {
    assert!(g.tabs[0].1 > g.tabs[1].1 && g.tabs[0].1 > g.tabs[2].1);
    assert!(g.floor_gib > EMERGENCY_FREE_GIB && g.warn_gib > g.floor_gib);
    assert!(g.image_gib * GIB / 100 * EMERGENCY_FREE_PCT < EMERGENCY_FREE_GIB * GIB);
    assert!(g.laid_free < g.floor() && g.laid_free > EMERGENCY_FREE_GIB * GIB);
    let need = g.tab(0) + g.tab(1) + g.tab(2) + g.laid_free + HEADROOM;
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
    tabs: [("alpha", 7), ("beta", 2), ("gamma", 1)],
    silent: [false; 3],
};

/// `partial`: the floor raised to 8 GiB, so the volume laid just past the emergency line can rise
/// by the first tab's build, past [`ask::FREED_MIN`], and stay below it.
const PARTIAL: Geometry = Geometry {
    image_gib: 10,
    floor_gib: 8,
    warn_gib: 9,
    laid_free: 46 * GIB / 10,
    tabs: [("alpha", 2), ("beta", 1), ("gamma", 1)],
    silent: [false; 3],
};

/// `unanswered`: the first tab answers with words alone; the second, whose build is larger than
/// the third's by whole GiB, so the round after the hand-off elects it, frees past
/// [`ask::FREED_MIN`] with the volume still below its floor, so the episode is settled.
const UNANSWERED: Geometry = Geometry {
    image_gib: 12,
    floor_gib: 8,
    warn_gib: 9,
    laid_free: 46 * GIB / 10,
    tabs: [("alpha", 3), ("beta", 2), ("gamma", 1)],
    silent: [true, false, false],
};

/// `exhausted`: `unanswered`'s sizes with its second tab silent too, so the episode spends its
/// [`ask::MAX_ASKS`] lines on the two largest builds, freeing nothing; the third tab's is the build
/// a line past the cap would name.
const EXHAUSTED: Geometry = Geometry {
    silent: [true, true, false],
    ..UNANSWERED
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
}

impl Scenario {
    const ALL: [Scenario; 4] = [
        Scenario::Clear,
        Scenario::Partial,
        Scenario::Unanswered,
        Scenario::Exhausted,
    ];

    const fn word(self) -> &'static str {
        match self {
            Scenario::Clear => "clear",
            Scenario::Partial => "partial",
            Scenario::Unanswered => "unanswered",
            Scenario::Exhausted => "exhausted",
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
        }
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
}

/// The `free-ask` rows of the ledger `text` (`disk.jsonl`), oldest first.
fn ask_rows(text: &str) -> Vec<AskRow> {
    text.lines()
        .filter_map(|l| J::parse(l).ok())
        .filter(|r| r.get("kind").and_then(J::as_str) == Some(ask::ROW_KIND))
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(J::as_str).unwrap_or("").to_owned();
            AskRow {
                ts: s("ts"),
                said: s("said"),
                sid: s("sid"),
                step: s("step"),
                why: s("why"),
                text: s("text"),
                free: r
                    .get("free_bytes")
                    .and_then(J::as_i64)
                    .and_then(|f| u64::try_from(f).ok()),
            }
        })
        .collect()
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

/// The stub's mode: [`WORKING`] to anything; to a message holding the disk line, in a silent
/// tab's conversation (its token in a prompt) [`DECLINED`] alone, and in any other a Bash call of
/// [`CLEANUP`], then [`REMOVED`] once its result is back.
fn stub_mode(tabs: &[Tab]) -> J {
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
    t0: Instant,
    /// The ledger's rows printed so far.
    printed: Cell<usize>,
}

impl Lane<'_> {
    fn rows(&self) -> Vec<AskRow> {
        ask_rows(&crate::util::scratch_text(&self.ledger))
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
/// come back, and check the rise and that its `target/` is gone; with `short`, also that the
/// volume stays below its floor. The rise's moment, and free space then.
fn rise(
    lane: &Lane<'_>,
    v: &mut Verdict,
    tab: &Tab,
    free_before: u64,
    short: bool,
) -> Result<(Instant, u64), Fail> {
    let build = tab.gib * GIB;
    let rose = wait_for(RISE + SLACK, || {
        lane.show();
        lane.free() >= free_before + build * 9 / 10
    })?;
    let t_rise = Instant::now();
    let after = lane.free();
    let gone = !tab.dir.join("target").exists();
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

/// aterm's disk worker, which the window starts where no tab can free the volume in time
/// (`ask::worker_due`, the design's §8.4): wait for a look past the ledger's first `skip` rows to
/// claim it (a `worker` row; `due` says why it is due) and for it to be over (`worker-over`), and
/// check that while it was claimed or running no round elected anyone and no tab was typed a line
/// (each tab still at `want` lines, read off every transcript), and that it ran in a session of
/// its own: no worker row and no lease names a tab's session as the worker's, and no tab's
/// conversation holds its task. Where the rounds read on from: the rows past its `worker-over`, or
/// `None` when it was never claimed or never over.
fn worker(
    lane: &Lane<'_>,
    v: &mut Verdict,
    skip: usize,
    due: &str,
    want: &[usize],
) -> Result<Option<usize>, Fail> {
    let claimed = lane
        .wait_row(TICK * 2 + SLACK, skip, |r| r.said == "worker")?
        .is_some();
    v.check(
        &format!("{due}, so a look claimed aterm's disk worker (a `worker` row)"),
        claimed,
        || format!("rows: {:?}", lane.rows()),
    );
    if !claimed {
        return Ok(None);
    }
    let t_claim = Instant::now();
    println!("  worker    claimed {}", lane.since(t_claim));
    let place = |rows: &[AskRow], from: usize, said: &str| {
        rows.iter()
            .enumerate()
            .skip(from)
            .find(|(_, r)| r.said == said)
            .map(|(i, _)| i)
    };
    let at = place(&lane.rows(), skip, "worker").unwrap_or(skip);
    // A hang detector of the claim's longest life: its spawn, then its run.
    let life = Duration::from_millis((ask::SPAWN_MS + ask::WORKER_MS).unsigned_abs());
    let over = lane.wait_row(life + TICK * 2 + SLACK, at, |r| r.said == "worker-over")?;
    let got = lane.received();
    let rows = lane.rows();
    let end = place(&rows, at, "worker-over").unwrap_or(rows.len());
    if let Some(o) = &over {
        println!(
            "  worker    {}over why={} {}",
            if o.sid.is_empty() {
                String::new()
            } else {
                format!("@{} ", o.sid)
            },
            o.why,
            lane.since(Instant::now())
        );
    }
    let mut sids = worker_sids(&rows, at);
    for e in &lane.lease().volumes {
        if let Some(s) = e.worker.as_ref().and_then(ask::Worker::sid)
            && !sids.iter().any(|x| x == s)
        {
            sids.push(s.to_owned());
        }
    }
    let tabs = tabs_among(&sids, &lane.tabs);
    println!(
        "  worker    ran as {}",
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
    Ok(over.map(|_| end + 1))
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
    Ok(())
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
    .unwrap_or(at);
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

/// Run scenario `sc` on the lab `tag` that [`cmd_disk_ask`] brought up.
fn lane(tag: &str, sc: Scenario, v: &mut Verdict) -> Result<(), Fail> {
    let g = sc.geometry();
    let run = Run::load(tag)?;
    let door = Door::new(&run)?;
    let home = std::fs::canonicalize(run.w.join("home"))
        .map_err(|e| Fail::die(format!("the scratch home: {e}")))?;
    println!(
        "disk-ask: scenario {}, lab '{tag}', Claude Code {}, {}; HOME on its {} image at {}",
        sc.word(),
        run.claude_ver,
        run.need_aterm()?.display(),
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
            silent,
        })
        .collect();
    for t in &tabs {
        lay_source(&t.dir)?;
        lay_target_sized(&t.dir.join("target"), t.gib * GIB, 1 << 20)?;
    }
    let free = free_on(&home).ok_or("the image's free space could not be read")?;
    if free > g.laid_free {
        fill(&home.join("ballast/fill"), free - g.laid_free)?;
    }
    let laid = free_on(&home).unwrap_or(0);
    println!(
        "  laid      {} (each a recent build in target/debug/deps), ballast to {} MiB free (floor {} MiB){}",
        tabs.iter()
            .map(|t| format!("{} {} GiB", t.name, t.gib))
            .collect::<Vec<_>>()
            .join(", "),
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

    crate::verbs::settings_merge(
        &run,
        &J::obj([(
            "permissions",
            J::obj([("allow", J::Arr(vec![J::s(format!("Bash({CLEANUP})"))]))]),
        )]),
    )?;
    crate::verbs::set_custom_mode(&run, MODE, &stub_mode(&tabs))?;
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
    let lane = Lane {
        run: &run,
        door: &door,
        ledger: ledger(&run),
        home,
        tabs,
        floor: g.floor(),
        laid_free: g.laid_free,
        t0: Instant::now(),
        printed: Cell::new(0),
    };
    println!("  tabs up   {}", lane.since(lane.t0));
    let Some(h) = head(&lane, v)? else {
        return Ok(());
    };
    match sc {
        Scenario::Clear => clear(&lane, v, &h),
        Scenario::Partial => partial(&lane, v, &h),
        Scenario::Unanswered => unanswered(&lane, v, &h),
        Scenario::Exhausted => exhausted(&lane, v, &h),
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

/// `disk-ask [tag] [--keep] [--scenario clear|partial|unanswered|exhausted]`.
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
        let picked = crate::stub::with_reply(stub_mode(&tabs), &users(&[task]));
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
        let spec = stub_mode(&[tab("alpha"), tab("beta"), tab("gamma")]);
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
        let spec = stub_mode(&[alpha, tab("beta"), tab("gamma")]);
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
        }
    }
}
