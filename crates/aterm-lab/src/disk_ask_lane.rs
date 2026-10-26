// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `disk-ask [tag] [--keep]`: the ask to free disk space (`disk-free@v1`,
//! `docs/DESIGN-disk-free-ask-2026-10-24.md`) end to end, on a real aterm with real Claude Code.
//!
//! A jailed lab of its own ([`crate::lab::up`]) whose `HOME` is a sparse APFS image
//! (`LAB_HOME_IMAGE`), its window the owner's disk watch there under the watch's development seam
//! ([`aterm_agent::harness::disk::WATCH_SEAM`]). The home holds one checkout per tab, each with a
//! recent build that aterm's own pass leaves (its bytes in `deps/`), and ballast that is no build,
//! so the volume stays below its floor once aterm's pass has done what it can. Three Claude Code
//! tabs, each in its own checkout and given a task, rest under the harness. The stub answers the
//! disk line, and only it, with a Bash call that removes the tab's `target/`. The lane holds that
//! exactly one tab is typed the line, the largest build's, which the line names as that tab's own
//! (read off the tab's working directory), that the volume rises after its tool call, and that
//! the ledger and `aterm harness disk why` credit that tab. Then it fills the volume below its
//! floor again and holds that the next looks open no episode, the volume's last line within the
//! cooldown ([`ask::COOLDOWN_MS`]), and that no tab is typed a second line. What each tab received
//! is read off every scratch transcript, so a line in a conversation no tab's token names fails.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::disk::ask;

use crate::LAB;
use crate::disk_lane::{Verdict, fill, free_on, lay_source, lay_target_sized};
use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::J;
use crate::run::{Run, is_run};
use crate::util::atomic_write;
use crate::workers::kv;
use crate::{Fail, Res, stop};

/// The home image's size.
const IMAGE: &str = "16g";

/// The lane's `[disk]`: the floor (`auto_free_gib`) above the emergency line (4 GiB), so the
/// volume reads `short` between them, and the warning a GiB above the floor.
const DISK_TOML: &str = "[disk]\nwarn_free_gib = 6\nauto_free_gib = 5\nidle_build_dirs = true\n";

/// The floor [`DISK_TOML`] writes.
const FLOOR: u64 = 5 << 30;

/// Where free space sits once the home is laid: below the floor, past the emergency line
/// (4 GiB), with room for what three Claude Code sessions write in their homes meanwhile.
const LAID_FREE: u64 = (46 << 30) / 10;

/// The tabs: each one's checkout and the build it holds, in GiB. The first holds the most, so
/// the election (`ask::rank`, the whole GiB a session's own checkout holds first) picks it, and
/// its build alone lifts the volume past the floor plus [`ask::END_MARGIN`].
const TABS: [(&str, u64); 3] = [("alpha", 7), ("beta", 2), ("gamma", 1)];

// The geometry the lane stands on: the first tab's checkout holds the most, so the election picks
// it; the home is laid below its floor and past the emergency line; and removing the first tab's
// build alone ends the episode, past the floor and its margin, with a GiB to spare.
const _: () = {
    assert!(TABS[0].1 > TABS[1].1 && TABS[0].1 > TABS[2].1);
    assert!(LAID_FREE < FLOOR && LAID_FREE > aterm_agent::harness::disk::EMERGENCY_FREE_GIB << 30);
    assert!(LAID_FREE + (TABS[0].1 << 30) >= FLOOR + ask::END_MARGIN + (1 << 30));
};

/// What the host keeps free past the image before the lane lays anything.
const HOST_SPARE: u64 = 8 << 30;

/// The custom stub mode the tabs talk to.
const MODE: &str = "diskask";

/// What each tab's agent says to anything but the disk line.
const WORKING: &str = "Working on it: this checkout's build comes next.";

/// What the asked tab's agent says once its tool call ran.
const REMOVED: &str = "Removed this checkout's build directory (target/) to free space.";

/// The command the asked tab's agent runs (in its own checkout).
const CLEANUP: &str = "rm -rf target";

/// The disk watch's first look after the window starts, and the period of its looks (the
/// window's `DISK_FIRST_LOOK` and `DISK_TICK`).
const FIRST_LOOK: Duration = Duration::from_secs(120);
const TICK: Duration =
    Duration::from_millis(aterm_agent::supervise::occupancy::reading::LOOK_EVERY_MS.unsigned_abs());

/// Slack on every hang detector: a look's pass, a turn break's wait, a busy host.
const SLACK: Duration = Duration::from_secs(90);

/// How long the lane watches the volume short again, past the look that read it so and opened no
/// episode, for a second line: two more looks and half of one.
const QUIET: Duration = Duration::from_secs(5 * 30);

/// One tab of the lane.
#[derive(Debug, Clone)]
struct Tab {
    name: &'static str,
    gib: u64,
    token: String,
    dir: PathBuf,
    sid: String,
}

/// One `free-ask` row of the ledger, as the lane reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AskRow {
    ts: String,
    said: String,
    sid: String,
    step: String,
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
                text: s("text"),
                free: r
                    .get("free_bytes")
                    .and_then(J::as_i64)
                    .and_then(|f| u64::try_from(f).ok()),
            }
        })
        .collect()
}

/// The ledger the lab's window writes its disk rows to.
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

    /// The tabs that received a line.
    fn got(&self) -> Vec<&str> {
        self.tabs
            .iter()
            .filter(|(_, l)| l.as_ref().is_some_and(|l| !l.is_empty()))
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// The tabs no transcript was found for.
    fn unread(&self) -> Vec<&str> {
        self.tabs
            .iter()
            .filter(|(_, l)| l.is_none())
            .map(|(n, _)| n.as_str())
            .collect()
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
}

/// The disk-ask lines `all` (every transcript, with its file) holds: each user row that is no
/// tool's result and holds the rule's tag, counted for the tab whose token the transcript's first
/// prompt holds (every such transcript of the tab), or as stray, with its file, when that prompt
/// holds no tab's token.
fn received_in(all: &[(PathBuf, Vec<crate::workers::Row>)], tabs: &[Tab]) -> Received {
    let mut out = Received {
        tabs: tabs.iter().map(|t| (t.name.to_owned(), None)).collect(),
        stray: Vec::new(),
    };
    for (file, rows) in all {
        let lines = rows
            .iter()
            .filter(|r| {
                r.kind == "user" && r.result.is_none() && r.text.contains(ask::RULE_DISK_FREE)
            })
            .map(|r| r.text.clone());
        let tab = crate::workers::first_prompt(rows)
            .and_then(|p| tabs.iter().position(|t| p.text.contains(&t.token)));
        match tab {
            Some(i) => out.tabs[i].1.get_or_insert_with(Vec::new).extend(lines),
            None => out.stray.extend(lines.map(|l| (file.clone(), l))),
        }
    }
    out
}

/// Whether `line` names tab `name`'s checkout's builds marked the asked tab's own, alone in its
/// checkout: `~/work/<name>'s builds <size> (this tab's)`.
fn marks_own(line: &str, name: &str) -> bool {
    line.split_once(&format!("~/work/{name}'s builds "))
        .and_then(|(_, rest)| rest.split_once(')'))
        .is_some_and(|(said, _)| said.ends_with(" (this tab's") && !said.contains(';'))
}

/// [`received_in`] every scratch transcript of `run`.
fn received(run: &Run, tabs: &[Tab]) -> Received {
    received_in(&crate::workers::transcripts(run), tabs)
}

/// The stub's mode: [`WORKING`] to anything, and to a message holding the disk line, a Bash call
/// of [`CLEANUP`], then [`REMOVED`] once its result is back.
fn stub_mode() -> J {
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
    J::obj([
        ("stream", J::Bool(true)),
        ("text", J::s(WORKING)),
        (
            "replies",
            J::Arr(vec![J::obj([
                ("last", J::s(ask::RULE_DISK_FREE)),
                ("tool", call),
                ("text", J::s(REMOVED)),
            ])]),
        ),
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

/// Run the lane on the lab `tag` that [`cmd_disk_ask`] brought up.
#[allow(clippy::too_many_lines)]
fn lane(tag: &str, v: &mut Verdict) -> Result<(), Fail> {
    let run = Run::load(tag)?;
    let door = Door::new(&run)?;
    let t0 = Instant::now();
    let since = |at: Instant| format!("+{:.0}s", at.duration_since(t0).as_secs_f64());
    let home = std::fs::canonicalize(run.w.join("home"))
        .map_err(|e| Fail::die(format!("the scratch home: {e}")))?;
    println!(
        "disk-ask: lab '{tag}', Claude Code {}, {}; HOME on its image at {}",
        run.claude_ver,
        run.need_aterm()?.display(),
        home.display()
    );
    let mut nonce = [0u8; 4];
    let _ = aterm_uds::rand::fill(&mut nonce);
    let nonce = format!("{:08x}", u32::from_ne_bytes(nonce));
    let mut tabs: Vec<Tab> = TABS
        .iter()
        .map(|(name, gib)| Tab {
            name,
            gib: *gib,
            token: format!("LABDISK-{name}-{nonce}"),
            dir: home.join("work").join(name),
            sid: String::new(),
        })
        .collect();
    for t in &tabs {
        lay_source(&t.dir)?;
        lay_target_sized(&t.dir.join("target"), t.gib << 30, 1 << 20)?;
    }
    let free = free_on(&home).ok_or("the image's free space could not be read")?;
    if free > LAID_FREE {
        fill(&home.join("ballast/fill"), free - LAID_FREE)?;
    }
    let laid = free_on(&home).unwrap_or(0);
    println!(
        "  laid      {} (each a recent build in target/debug/deps), ballast to {} MiB free (floor {} MiB) {}",
        tabs.iter()
            .map(|t| format!("{} {} GiB", t.name, t.gib))
            .collect::<Vec<_>>()
            .join(", "),
        laid >> 20,
        FLOOR >> 20,
        since(Instant::now())
    );
    v.check(
        "the home is laid below its floor and past the emergency line",
        ((4 << 30)..FLOOR).contains(&laid),
        || format!("{} MiB free", laid >> 20),
    );

    crate::verbs::settings_merge(
        &run,
        &J::obj([(
            "permissions",
            J::obj([("allow", J::Arr(vec![J::s(format!("Bash({CLEANUP})"))]))]),
        )]),
    )?;
    crate::verbs::set_custom_mode(&run, MODE, &stub_mode())?;
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
    let t_up = Instant::now();
    println!("  tabs up   {}", since(t_up));

    let ledger = ledger(&run);
    let rows = || ask_rows(&crate::util::scratch_text(&ledger));
    let first = |said: &str| rows().into_iter().find(|r| r.said == said);
    let mut printed = 0usize;
    let mut show = || {
        let all = rows();
        for r in all.iter().skip(printed) {
            println!(
                "  row       {} {}{} {}{}",
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
                r.free
                    .map_or_else(String::new, |f| format!("free={} MiB", f >> 20))
            );
        }
        printed = all.len();
    };

    let opened = wait_for(FIRST_LOOK + TICK * 3 + SLACK, || {
        show();
        first("opened").is_some()
    })?;
    v.check(
        "an episode opened once aterm's own pass was spent",
        opened,
        || {
            format!(
                "no `opened` row within {} s of the window's start; rows: {:?}",
                (FIRST_LOOK + TICK * 3 + SLACK).as_secs(),
                rows()
            )
        },
    );
    if !opened {
        return Ok(());
    }
    let t_open = Instant::now();
    println!("  opened    {}", since(t_open));
    let nominate = Duration::from_millis(ask::NOMINATE_MS.unsigned_abs());
    let elected = wait_for(nominate + TICK * 2 + SLACK, || {
        show();
        first("elected").is_some()
    })?;
    let pick = first("elected").map(|r| r.sid).unwrap_or_default();
    let biggest = tabs[0].clone();
    v.check(
        &format!(
            "the round elected {} (@{}), whose checkout holds the most",
            biggest.name, biggest.sid
        ),
        elected && pick == biggest.sid,
        || format!("elected={elected} pick=@{pick}; rows: {:?}", rows()),
    );
    if !elected {
        return Ok(());
    }
    let t_elected = Instant::now();
    println!("  elected   @{pick} {}", since(t_elected));
    let free_before = free_on(&home).unwrap_or(0);
    let typed = wait_for(TICK * 2 + SLACK, || {
        show();
        received(&run, &tabs).total() > 0
    })?;
    let t_line = Instant::now();
    let got = received(&run, &tabs);
    let say = |got: &Received| {
        for (name, lines) in &got.tabs {
            match lines {
                Some(l) => println!("  received  {name}: {} line(s)", l.len()),
                None => println!("  received  {name}: no transcript of it"),
            }
        }
        for (file, _) in &got.stray {
            println!("  received  a line in {}, no tab's", file.display());
        }
    };
    say(&got);
    v.check("the disk line reached a tab's conversation", typed, || {
        format!(
            "none within {} s of the election",
            (TICK * 2 + SLACK).as_secs()
        )
    });
    if !typed {
        return Ok(());
    }
    let line = got.first().unwrap_or_default().to_owned();
    let chars = line.chars().count();
    println!("  line      ({chars} chars) {}", line.replace('\n', " "));
    // Its holder: the asked tab's own build, marked so, which the line can say only when the
    // asked session's working directory read its checkout (the image is mounted at the scratch
    // home, a long path, so the volume's name gives way to it: the design's §4).
    v.check(
        &format!(
            "the line is the rule's: its tag, {}'s build marked this tab's, only this tab asked, within its cap",
            biggest.name
        ),
        line.contains(&format!("(rule {})", ask::RULE_DISK_FREE))
            && marks_own(&line, biggest.name)
            && line.contains("Only this tab was asked.")
            && chars <= ask::LINE_CAP,
        || format!("{chars} chars (cap {}): {line}", ask::LINE_CAP),
    );
    let rose = wait_for(Duration::from_secs(60) + SLACK, || {
        show();
        free_on(&home).is_some_and(|f| f >= free_before + ((biggest.gib << 30) * 9 / 10))
    })?;
    let t_rise = Instant::now();
    let free_after = free_on(&home).unwrap_or(0);
    println!(
        "  freed     {} MiB -> {} MiB free {}",
        free_before >> 20,
        free_after >> 20,
        since(t_rise)
    );
    v.check(
        &format!(
            "the volume rose by {} GiB after the asked tab's tool call (its target/ gone)",
            biggest.gib
        ),
        rose && !biggest.dir.join("target").exists(),
        || {
            format!(
                "{} MiB -> {} MiB, target/ there: {}",
                free_before >> 20,
                free_after >> 20,
                biggest.dir.join("target").exists()
            )
        },
    );
    let credited = |r: &AskRow| r.said == "freed" && r.sid == biggest.sid;
    let settled = wait_for(TICK * 2 + SLACK, || {
        show();
        rows().iter().any(credited)
    })?;
    v.check(
        &format!(
            "the ledger credits @{} with the space (a `freed` row)",
            biggest.sid
        ),
        settled,
        || format!("rows: {:?}", rows()),
    );
    let t_settle = Instant::now();
    let ended = wait_for(TICK * 2 + SLACK, || {
        show();
        rows()
            .iter()
            .skip_while(|r| !credited(r))
            .any(|r| r.said == "ended")
    })?;
    v.check(
        "a later look read the volume clear and ended the episode (an `ended` row after the credit)",
        ended,
        || format!("rows: {:?}", rows()),
    );
    let t_ended = Instant::now();
    // The volume short again within the cooldown of the line: a look reads it so, aterm's own pass
    // spent, and opens no episode, so nobody is asked again. A second line would come here, if
    // anywhere: an episode that opened would elect within a round and type at the next break.
    let clear = free_on(&home).unwrap_or(0);
    if clear > LAID_FREE {
        fill(&home.join("ballast/refill"), clear - LAID_FREE)?;
    }
    let short = free_on(&home).unwrap_or(0);
    let refilled_at = rows().len();
    println!(
        "  refilled  {} MiB -> {} MiB free (floor {} MiB) {}",
        clear >> 20,
        short >> 20,
        FLOOR >> 20,
        since(Instant::now())
    );
    v.check(
        "the volume is filled below its floor again",
        short < FLOOR,
        || format!("{} MiB free", short >> 20),
    );
    let since_refill = |said: &str| {
        rows()
            .into_iter()
            .skip(refilled_at)
            .filter(|r| r.said == said)
            .count()
    };
    let cooled = wait_for(TICK * 2 + SLACK, || {
        show();
        since_refill("cooldown") > 0 || since_refill("opened") > 0
    })?;
    v.check(
        "a look read the volume short again and opened no episode, the line within the cooldown (a `cooldown` row)",
        cooled && since_refill("cooldown") > 0 && since_refill("opened") == 0,
        || format!("rows since the refill: {:?}", &rows()[refilled_at.min(rows().len())..]),
    );
    let t_cooled = Instant::now();
    let _ = wait_for(QUIET, || {
        show();
        false
    })?;
    let count = |said: &str, step: &str| {
        rows()
            .iter()
            .filter(|r| r.said == said && r.step == step)
            .count()
    };
    let still = free_on(&home).unwrap_or(0);
    v.check(
        &format!(
            "no second episode or line over {} s more of the volume short ({} MiB free at the end): one `opened`, one `elected`, one `typed`",
            QUIET.as_secs(),
            still >> 20
        ),
        still < FLOOR
            && count("opened", "") == 1
            && count("elected", "") == 1
            && count("step", "typed") == 1,
        || format!("rows: {:?}", rows()),
    );
    let got = received(&run, &tabs);
    say(&got);
    v.check(
        "every tab's conversation was read (a transcript whose first prompt holds its token)",
        got.unread().is_empty(),
        || format!("none for {:?}", got.unread()),
    );
    v.check(
        &format!(
            "exactly one tab, {}, received the disk line, once, over the whole lane, read off every transcript",
            biggest.name
        ),
        got.total() == 1 && got.got() == [biggest.name] && got.stray.is_empty(),
        || format!("{got:?}"),
    );
    let why = door.harness(&["disk", "why"])?;
    println!("  why       (exit {})", why.code);
    for l in why.stdout.lines() {
        println!("    | {l}");
    }
    v.check(
        &format!("`aterm harness disk why` names @{}'s freeing", biggest.sid),
        why.code == 0
            && why
                .stdout
                .lines()
                .any(|l| l.contains("freed") && l.contains(&format!("@{}", biggest.sid))),
        || format!("{}{}", why.stdout, why.stderr),
    );
    let secs = |a: Instant, b: Instant| b.saturating_duration_since(a).as_secs();
    println!(
        "  summary   opened {} s after the tabs were up, elected {} s later, the line {} s after the election, the rise {} s after the line, {}, ended {} s later; refilled short, {} s to the look that opened nothing, then {} s quiet",
        secs(t_up, t_open),
        secs(t_open, t_elected),
        secs(t_elected, t_line),
        secs(t_line, t_rise),
        if settled {
            format!("the credit {} s after the rise", secs(t_rise, t_settle))
        } else {
            format!("no credit within {} s of the rise", secs(t_rise, t_settle))
        },
        secs(t_settle, t_ended),
        secs(t_ended, t_cooled),
        QUIET.as_secs()
    );
    Ok(())
}

/// The lane's lab, up: taken down when it drops ([`crate::lab::down`]), so a lane that panics or
/// returns early still ends its instance and detaches its home image, which holds the builds and
/// ballast it laid (some 11 GiB of the host's space).
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

/// `disk-ask [tag] [--keep]`.
/// # Errors
/// A usage error, an interrupt, or a lab that could not be read.
pub fn cmd_disk_ask(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("disk-ask: {m}"));
    let (mut tag, mut keep) = (None::<String>, false);
    for a in args {
        match a.as_str() {
            "--keep" => keep = true,
            s if s.starts_with('-') => {
                return Err(usage(&format!("unknown option '{s}' (--keep)")));
            }
            s => tag = Some(s.to_string()),
        }
    }
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
    let need = crate::lab::home_image_bytes(IMAGE).map_err(Fail::die)? + HOST_SPARE;
    match free_on(&crate::paths::paths().state) {
        Some(f) if f >= need => {}
        free => {
            println!(
                "disk-ask: NOT RUN (skip: {} free here, the lane needs {} GiB so it cannot be what fills the disk)",
                free.map_or_else(|| "unknown".to_owned(), |f| format!("{} GiB", f >> 30)),
                need >> 30
            );
            return Ok(3);
        }
    }
    stop::install();
    let mut u = UpOpts::from_env();
    u.lifeline = !keep;
    u.home_image = Some(IMAGE.to_owned());
    u.aterm_toml = format!("{}\n{DISK_TOML}", u.aterm_toml);
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
    let ran = lane(&tag, &mut v);
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
        println!("disk-ask: PASS ({} checks)", v.rows.len());
        Ok(0)
    } else {
        println!("disk-ask: FAIL");
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
        );
        let rows = ask_rows(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].said.as_str(), rows[0].sid.as_str(), rows[0].free),
            ("elected", "s-1", Some(5))
        );
        assert_eq!(
            (rows[1].said.as_str(), rows[1].step.as_str()),
            ("step", "typed")
        );
    }

    fn tab(name: &'static str) -> Tab {
        Tab {
            name,
            gib: 1,
            token: format!("LABDISK-{name}-1"),
            dir: PathBuf::from(format!("/w/{name}")),
            sid: String::new(),
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
        assert_eq!(got.got(), ["alpha"]);
        assert_eq!(got.unread(), ["gamma"], "no transcript of gamma's");
        assert_eq!(got.first(), Some(line));
        let mut all = all;
        all.push((
            PathBuf::from("/p/c.jsonl"),
            vec![user("resumed"), user(line)],
        ));
        let got = received_in(&all, &tabs);
        assert_eq!(got.total(), 2, "{got:?}");
        assert_eq!(got.got(), ["alpha"]);
        assert_eq!(got.stray, [(PathBuf::from("/p/c.jsonl"), line.to_owned())]);
        // A tab's second transcript counts for it.
        all.push((
            PathBuf::from("/p/a2.jsonl"),
            vec![user("LABDISK-alpha-1: again"), user(line)],
        ));
        let got = received_in(&all, &tabs);
        assert_eq!(got.tabs[0].1.as_ref().map(Vec::len), Some(2), "{got:?}");
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

    #[test]
    fn the_stub_answers_only_the_disk_line_with_the_cleanup() {
        let spec = stub_mode();
        let users = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
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
}
