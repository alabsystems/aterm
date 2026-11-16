// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `sys disk held`'s lines and JSON over rows as the host gathers them: each holding with its
//! session's mark and its next pass, the rows balanced against the volume's used bytes, and
//! each path kept to the caller's tab.

use super::*;
use crate::sys::json::contract_violations;
use crate::sys::{
    BudgetFacts, BudgetRead, Freshness, Machine, Reply, SysQuery, TabFact, Target, render_json,
    render_text,
};

const REPO: &str = "/Users//a/w/repo";
const TARGET: &str = "/Users//a/w/repo/target";
const TWO: &str = "/Users//a/w/two";
const MIRROR: &str = "/Users//a/Library/Caches/aterm/.ubd/cargo-build.noindex/Users//a/w/repo";
const OLD: &str = "/Users//a/Library/Caches/aterm/.ubd/cargo-build.noindex/Users//a/w/old";
const STATE: &str = "/Users//a/Library/Application Support/aterm";
const OPT_IN: &str = "/Users//a/Documents/x";
/// Tab 1's one pane.
const MINE: &[u64] = &[7];
/// Tab 2's one pane.
const THEIRS: &[u64] = &[8];

struct World {
    machine: Machine,
    target: Target,
    tabs: Vec<TabFact>,
    volume: VolumeFacts,
    held: Holdings,
}

/// Two tabs (pane 7 in tab 1, pane 8 in tab 2) on a volume of 5,000 bytes, `used` of them
/// used, holding `rows`, the oldest walk 4 s old.
fn world(rows: Vec<Holding>, used: u64) -> World {
    World {
        machine: Machine::default(),
        target: Target {
            key: 7,
            sid: "s-ab12".into(),
            window: Some(1),
            tab: Some(1),
            panes: vec![7],
            window_sessions: vec![7, 8],
            ..Target::default()
        },
        tabs: vec![
            TabFact {
                key: 7,
                sid: "s-ab12".into(),
                window: Some(1),
                tab: Some(1),
            },
            TabFact {
                key: 8,
                sid: "s-cd34".into(),
                window: Some(1),
                tab: Some(2),
            },
        ],
        volume: VolumeFacts {
            mount: "/System/Volumes/Data".into(),
            fs: "apfs".into(),
            total: 5_000,
            free: 5_000 - used,
            reserved: 0,
            level: Some("ok".into()),
            budget: BudgetRead::Sized(BudgetFacts {
                cap: 4_000,
                cap_written: false,
                kept: 600,
                target: 3_400,
                rebuildable: 400,
                partial: false,
                sized_s: 30,
            }),
        },
        held: Holdings {
            rows,
            walk: Walk::Complete,
            oldest_walk_s: Some(4),
        },
    }
}

/// The context of a request over `w` from a caller in the tab of `caller` (or none).
fn ctx<'a>(w: &'a World, s: &'a Snapshot, caller: Option<&'a [u64]>, owner: bool) -> Ctx<'a> {
    Ctx {
        snap: Some(s),
        epoch: 0x7f3a,
        as_of_unix_ms: 1_792_185_697_412,
        age_ms: 120,
        state: Freshness::Fresh,
        machine: &w.machine,
        target: &w.target,
        tabs: &w.tabs,
        caller_panes: caller,
        owner_class: owner,
        volume: Some(&w.volume),
        roots: &[],
        disk: None,
        stores: &[],
        home: Some("/Users//a"),
        events: &[],
        gap: None,
        head_seq: 0,
        held: Some(&w.held),
    }
}

/// A snapshot whose used bytes grew 2.5 bytes a second over the last hour.
fn snap() -> Snapshot {
    Snapshot {
        facts: crate::model::RowFacts {
            disk_rates: crate::history::Rates {
                rate60: Some(2.5),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Snapshot::default()
    }
}

/// A row at `path`, its last component its project: a checkout no session claims, kept.
fn row(path: &str, bytes: Bytes) -> Holding {
    Holding {
        project: path.rsplit('/').next().unwrap_or(path).to_owned(),
        worktree: Worktree::Checkout,
        path: path.to_owned(),
        keys: Vec::new(),
        mark: Mark::NoSession,
        tab: None,
        exe: None,
        bytes,
        class: Class::Kept,
        next: Next::Nothing,
    }
}

/// `h`, claimed by pane `key`, whose session is at work in tab `tab` running claude.
fn at_work(h: Holding, key: u64, tab: usize) -> Holding {
    Holding {
        keys: vec![key],
        mark: Mark::AtWork,
        tab: Some(tab),
        exe: Some("claude".into()),
        ..h
    }
}

/// A gone session's build output in aterm's build cache.
fn gone(path: &str, bytes: Bytes) -> Holding {
    Holding {
        worktree: Worktree::Build,
        mark: Mark::Gone,
        class: Class::Rebuildable,
        next: Next::DailyCache { stale_days: 3 },
        ..row(path, bytes)
    }
}

/// The value of `key=` in a record line, before its `next:` statement.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let head = line.split_once(" next: ").map_or(line, |(head, _)| head);
    let value = |t: &'a str| t.strip_prefix(key)?.strip_prefix('=');
    head.split(' ').find_map(value)
}

/// A `holding` line's `bytes=` as a number: a floor's figure, 0 where it is unavailable.
fn bytes_of(line: &str) -> u64 {
    let Some(v) = field(line, "bytes") else {
        panic!("no bytes= in {line}");
    };
    if v.starts_with("unavailable(") {
        return 0;
    }
    v.trim_start_matches('\u{2265}')
        .parse()
        .unwrap_or_else(|e| panic!("{line}: {e}"))
}

/// The text of the line keyed `key`.
fn line_of<'r>(r: &'r Reply, key: &str) -> &'r str {
    r.lines
        .iter()
        .find(|l| l.key == key)
        .map(|l| l.text.as_str())
        .unwrap_or_else(|| panic!("no {key} line: {:#?}", r.lines))
}

/// The `holding` lines of a reply.
fn holdings(r: &Reply) -> Vec<String> {
    r.lines
        .iter()
        .filter(|l| l.key.starts_with("holding "))
        .map(|l| l.text.clone())
        .collect()
}

/// The `key=` value of each `holding` line of `r`, in order (`-` where a line has none).
fn column(r: &Reply, key: &str) -> Vec<String> {
    let value = |l: &String| field(l, key).unwrap_or("-").to_owned();
    holdings(r).iter().map(value).collect()
}

/// The `holding` line of `project` among `lines`.
fn row_of<'l>(lines: &'l [String], project: &str) -> &'l str {
    let Some(found) = lines.iter().find(|l| field(l, "project") == Some(project)) else {
        panic!("no {project} row: {lines:#?}");
    };
    found
}

/// The `disk.held.rows` array of a `sys disk held --json` reply.
fn json_rows(json: &Jv) -> &[Jv] {
    let held = json.get("disk").and_then(|d| d.get("held"));
    let Some(Jv::Arr(rows)) = held.and_then(|h| h.get("rows")) else {
        panic!("no disk.held.rows: {}", json.to_text());
    };
    rows
}

/// The `path` of the JSON row of `project`, `None` where the row has none.
fn json_path(rows: &[Jv], project: &str) -> Option<Jv> {
    let named = Jv::s(project);
    let Some(row) = rows.iter().find(|r| r.get("project") == Some(&named)) else {
        panic!("no {project} row: {rows:?}");
    };
    row.get("path").cloned()
}

/// Asserts that the `holding {n}` line of `r` says `session`, `tab` (claude's, where at work),
/// `class` and `next`.
fn says(r: &Reply, n: usize, session: &str, tab: Option<&str>, class: &str, next: Next) {
    let l = line_of(r, &format!("holding {n}"));
    assert_eq!(field(l, "n"), Some(n.to_string().as_str()), "{l}");
    assert_eq!(field(l, "session"), Some(session), "{l}");
    assert_eq!(field(l, "tab"), tab, "{l}");
    assert_eq!(field(l, "exe"), tab.map(|_| "claude"), "{l}");
    assert_eq!(field(l, "class"), Some(class), "{l}");
    assert_eq!(field(l, "pass"), Some(next.pass()), "{l}");
    let tail = l.split_once(" next: ").map(|(_, tail)| tail);
    assert_eq!(tail, Some(next.says().as_str()), "{l}");
}

/// Whether `line` reads as a warning or a request: a warning word or sign, `NEEDS YOU`, an
/// exclamation, a backtick, or a `next:` statement that names a command.
fn asks_or_warns(line: &str) -> bool {
    let next = line.split_once("next: ").map_or("", |(_, tail)| tail);
    line.to_ascii_lowercase().contains("warn")
        || line.contains('\u{26a0}')
        || line.contains("NEEDS YOU")
        || line.contains('!')
        || line.contains('`')
        || next.contains("aterm ctl")
}

/// SYS DISK HELD SAYS WHO HOLDS THE DISK AND WHAT HAPPENS NEXT (DC-09a): one row per holding,
/// largest first, each with its project, worktree, its session's mark (and, at work, its tab
/// and program), bytes, class and the pass that reaches it first, then the used bytes no row
/// holds; the header and the JSON name the form and no scope; the held line says where the
/// walks are, and the summary says so of each that is not complete; and no statement of any
/// pass, long, brief or short (the Disk lens's forms, DC-09b), nor any walk's brief, nor anything
/// else, warns or asks. NEGATIVE CONTROLS: plain `disk` over the same context prints no holding,
/// names no form and keeps its own lines; the detector finds a forged statement that names a
/// command.
#[test]
fn sys_disk_held_prints_each_holding_with_its_session_mark_and_next_pass() {
    let mirror = Holding {
        worktree: Worktree::Build,
        class: Class::Rebuildable,
        next: Next::RunEnd {
            stale_days: Some(3),
        },
        ..row(MIRROR, Bytes::Measured(200))
    };
    let run_end = mirror.next;
    let daily = Next::DailyCache { stale_days: 3 };
    let target = Holding {
        project: "repo".into(),
        worktree: Worktree::Build,
        class: Class::Rebuildable,
        next: Next::TierPass,
        ..row(TARGET, Bytes::AtLeast(150))
    };
    let state = Holding {
        worktree: Worktree::Aterm,
        ..row(STATE, Bytes::Measured(50))
    };
    let opt_in = Holding {
        worktree: Worktree::OptIn,
        next: Next::NotAterms,
        ..row(OPT_IN, Bytes::Unmeasured("protected"))
    };
    let rows = vec![
        at_work(row(REPO, Bytes::Measured(300)), 7, 1),
        at_work(mirror, 7, 1),
        at_work(target, 7, 1),
        gone(OLD, Bytes::AtLeast(100)),
        state,
        opt_in,
    ];
    let w = world(rows, 1_000);
    let s = snap();
    let c = ctx(&w, &s, None, true);
    let reply = render_text(&SysQuery::parse("disk held").unwrap(), &c);
    let r = &reply;
    let header = reply.header(&c, false);
    assert!(
        header.contains(" view=disk form=held ") && !header.contains(" scope="),
        "{header}"
    );
    for l in &reply.lines {
        assert!(!l.text.contains("scope="), "{}", l.text);
    }
    assert!(line_of(r, "space").starts_with("space "));
    let budget = line_of(r, "budget");
    assert!(budget.starts_with("budget cap_bytes=4000 "), "{budget}");
    assert_eq!(
        line_of(r, "held"),
        "held used_bytes=1000 total_bytes=5000 rows=7 filling_bytes_per_h=9000 walk=complete \
         oldest_walk_s=4"
    );
    says(r, 1, "at-work", Some("1"), "kept", Next::Nothing);
    says(r, 2, "at-work", Some("1"), "rebuildable", run_end);
    says(r, 3, "at-work", Some("1"), "rebuildable", Next::TierPass);
    says(r, 4, "gone", None, "rebuildable", daily);
    says(r, 5, "none", None, "kept", Next::Nothing);
    says(r, 6, "none", None, "kept", Next::NotAterms);
    says(r, 7, "none", None, "unclassified", Next::Unknown);
    for (n, pass) in [(2, "run-end"), (3, "tier"), (4, "daily-cache")] {
        let l = line_of(r, &format!("holding {n}"));
        assert_eq!(field(l, "pass"), Some(pass), "{l}");
    }
    let projects = ["repo", "repo", "repo", "old", "aterm", "x", "other"];
    assert_eq!(column(r, "project"), projects);
    let worktrees = [
        "checkout",
        "build",
        "build",
        "build",
        "aterm",
        "opt-in",
        "unclassified",
    ];
    assert_eq!(column(r, "worktree"), worktrees);
    let bytes = [
        "300",
        "200",
        "\u{2265}150",
        "\u{2265}100",
        "50",
        "unavailable(protected)",
        "200",
    ];
    assert_eq!(column(r, "bytes"), bytes);
    let state_path = "/Users//a/Library/Application%20Support/aterm";
    assert_eq!(field(line_of(r, "holding 5"), "path"), Some(state_path));
    for l in &reply.lines {
        assert!(!asks_or_warns(&l.text), "{}", l.text);
    }
    let summary = line_of(r, "summary");
    assert!(
        !summary.contains(" · "),
        "every walk is complete: {summary}"
    );
    for walk in [Walk::Walking, Walk::Idle, Walk::Stopped] {
        let mut ws = world(w.held.rows.clone(), 1_000);
        ws.held.walk = walk;
        let held = render_text(
            &SysQuery::parse("disk held").unwrap(),
            &ctx(&ws, &s, None, true),
        );
        assert_eq!(field(line_of(&held, "held"), "walk"), Some(walk.word()));
        let summary = line_of(&held, "summary");
        assert!(summary.contains(" · "), "{walk:?}: {summary}");
        for l in &held.lines {
            assert!(!asks_or_warns(&l.text), "{}", l.text);
        }
        let brief = walk
            .brief()
            .unwrap_or_else(|| panic!("{walk:?} has no brief"));
        assert!(brief.len() <= 28, "{walk:?}: {brief}");
        assert!(!asks_or_warns(brief), "{walk:?}: {brief}");
    }
    assert_eq!(Walk::Complete.brief(), None);
    assert_eq!(Walk::Complete.clause(), None);
    let every = [
        Next::Nothing,
        Next::NotAterms,
        Next::CleanupOff,
        Next::TierPass,
        Next::DailyCache { stale_days: 3 },
        Next::DailyUnused { after_s: 3_600 },
        Next::DailyUnused {
            after_s: 30 * 24 * 3_600,
        },
        Next::RunEnd {
            stale_days: Some(3),
        },
        Next::RunEnd { stale_days: None },
        Next::OutsideCaches { idle_days: 7 },
        Next::Unknown,
    ];
    for next in every {
        // Each variant is listed above: one added fails to build here until it is listed too.
        match next {
            Next::Nothing
            | Next::NotAterms
            | Next::CleanupOff
            | Next::TierPass
            | Next::DailyCache { .. }
            | Next::DailyUnused { .. }
            | Next::RunEnd { .. }
            | Next::OutsideCaches { .. }
            | Next::Unknown => {}
        }
        let statement = format!("holding n=1 pass={} next: {}", next.pass(), next.says());
        assert!(!asks_or_warns(&statement), "{statement}");
        let brief = format!("holding n=1 pass={} next: {}", next.pass(), next.brief());
        assert!(!asks_or_warns(&brief), "{brief}");
        assert!(next.brief().len() <= 40, "{next:?}: {}", next.brief());
        let short = format!("holding n=1 pass={} next: {}", next.pass(), next.short());
        assert!(!asks_or_warns(&short), "{short}");
        assert!(next.short().len() <= 16, "{next:?}: {}", next.short());
        assert!(next.short().len() <= next.brief().len(), "{next:?}");
    }
    let json = render_json(&SysQuery::parse("disk held --json").unwrap(), &c);
    let violations = contract_violations(&json);
    assert!(violations.is_empty(), "{violations:?}");
    assert_eq!(json.get("schema"), Some(&Jv::Int(2)));
    assert_eq!(json.get("form"), Some(&Jv::s("held")));
    assert!(json.get("scope").is_none(), "{}", json.to_text());
    assert_eq!(json_rows(&json).len(), holdings(r).len());
    assert!(!asks_or_warns(&json.to_text()), "{}", json.to_text());
    let plain = render_text(&SysQuery::parse("disk").unwrap(), &c);
    assert!(holdings(&plain).is_empty(), "CONTROL: {:#?}", plain.lines);
    let plain_header = plain.header(&c, false);
    assert!(!plain_header.contains(" form="), "CONTROL: {plain_header}");
    let plain_json = render_json(&SysQuery::parse("disk --json").unwrap(), &c);
    assert!(plain_json.get("form").is_none(), "CONTROL");
    for key in ["disk", "volume", "budget"] {
        let has = plain.lines.iter().any(|l| l.key == key);
        assert!(has, "CONTROL {key}: {:#?}", plain.lines);
    }
    let forged = "holding n=1 pass=none next: run `aterm ctl @self end` to free it";
    assert!(asks_or_warns(forged), "CONTROL");
    let bare = "holding n=1 pass=none next: aterm ctl end frees it";
    assert!(asks_or_warns(bare), "CONTROL");
}

/// Each shown row of `b` over `rows`: its path, its bytes and whether the clamp cut it.
fn cut<'r>(rows: &'r [Holding], b: &Balanced) -> Vec<(&'r str, u64, bool)> {
    b.rows
        .iter()
        .map(|s| (rows[s.i].path.as_str(), s.bytes, s.clamped))
        .collect()
}

/// THE ROWS ADD UP TO THE VOLUME'S USED BYTES (DC-09a): largest first, the unmeasured last
/// (after a measured row of none), equal bytes by path; each row cut to what the rows above it
/// leave (cloned files count once per copy), and the rest one unclassified row, so the printed
/// bytes sum to used exactly. NEGATIVE CONTROL: with used unknown nothing is cut, and rows
/// that hold more than used print more than it.
#[test]
fn sys_disk_held_rows_sum_to_used_with_one_unclassified_row() {
    let fits = vec![
        row("/a", Bytes::Measured(300)),
        row("/b", Bytes::Measured(200)),
        row("/c", Bytes::Unmeasured("walking")),
        row("/e", Bytes::Measured(50)),
        row("/d", Bytes::AtLeast(50)),
        row("/z", Bytes::Measured(0)),
    ];
    let b = balance(&fits, Some(1_000));
    let want = [
        ("/a", 300, false),
        ("/b", 200, false),
        ("/d", 50, false),
        ("/e", 50, false),
        ("/z", 0, false),
        ("/c", 0, false),
    ];
    assert_eq!(cut(&fits, &b), want);
    assert_eq!(b.other, Some(400));
    let over = vec![
        row("/a", Bytes::Measured(700)),
        row("/b", Bytes::Measured(600)),
    ];
    let b = balance(&over, Some(1_000));
    assert_eq!(cut(&over, &b), [("/a", 700, false), ("/b", 300, true)]);
    assert_eq!(b.other, Some(0));
    let b = balance(&over, None);
    let uncut = [("/a", 700, false), ("/b", 600, false)];
    assert_eq!(cut(&over, &b), uncut, "CONTROL: no used, nothing cut");
    assert_eq!(b.other, None, "CONTROL: no used, no other row");
    let s = snap();
    let q = SysQuery::parse("disk held").unwrap();
    for rows in [fits, over] {
        let w = world(rows, 1_000);
        let lines = holdings(&render_text(&q, &ctx(&w, &s, None, true)));
        let sum: u64 = lines.iter().map(|l| bytes_of(l)).sum();
        assert_eq!(sum, 1_000, "{lines:#?}");
        let unclassified = |l: &&String| field(l, "worktree") == Some("unclassified");
        assert_eq!(lines.iter().filter(unclassified).count(), 1, "{lines:#?}");
        let last = lines.last().and_then(|l| field(l, "project"));
        assert_eq!(last, Some("other"), "{lines:#?}");
    }
}

/// A HOLDING'S PATH PRINTS ONLY TO A CALLER IN ITS TAB (DC-09a): every row's project, worktree
/// and bytes print to any owner-class caller, and its path only to a caller in a tab of a
/// session that claims it; elsewhere where it lies (`under=`), and a gone session's row, which
/// no live session claims, prints none to a caller in a tab. A caller that is not the owner's
/// reads no row. NEGATIVE CONTROLS: with no caller every path prints; a caller in the other
/// tab reads the reverse.
#[test]
fn sys_disk_held_prints_no_path_outside_the_callers_tab() {
    let rows = vec![
        at_work(row(REPO, Bytes::Measured(300)), 7, 1),
        at_work(row(TWO, Bytes::Measured(200)), 8, 2),
        gone(OLD, Bytes::AtLeast(100)),
    ];
    let w = world(rows, 1_000);
    let s = snap();
    let q = SysQuery::parse("disk held").unwrap();
    let text_for = |caller| holdings(&render_text(&q, &ctx(&w, &s, caller, true)));
    let mine = text_for(Some(MINE));
    assert_eq!(field(row_of(&mine, "repo"), "path"), Some(REPO));
    for project in ["two", "old"] {
        let l = row_of(&mine, project);
        assert_eq!(field(l, "path"), None, "{l}");
        assert_eq!(field(l, "under"), Some("home"), "{l}");
        assert!(field(l, "bytes").is_some(), "{l}");
    }
    let jq = SysQuery::parse("disk held --json").unwrap();
    let json = render_json(&jq, &ctx(&w, &s, Some(MINE), true));
    let rows = json_rows(&json);
    let home = Some(Jv::obj([("under", Jv::s("home"))]));
    assert_eq!(json_path(rows, "repo"), Some(Jv::s(REPO)));
    assert_eq!(json_path(rows, "two"), home);
    assert_eq!(json_path(rows, "old"), home);
    assert_eq!(json_path(rows, "other"), None, "other has no path");
    let all = text_for(None);
    for (project, path) in [("repo", REPO), ("two", TWO), ("old", OLD)] {
        let l = row_of(&all, project);
        assert_eq!(field(l, "path"), Some(path), "CONTROL: {l}");
    }
    let theirs = text_for(Some(THEIRS));
    let repo = row_of(&theirs, "repo");
    assert_eq!(field(repo, "path"), None, "CONTROL: {repo}");
    assert_eq!(field(repo, "under"), Some("home"), "CONTROL: {repo}");
    let two = row_of(&theirs, "two");
    assert_eq!(field(two, "path"), Some(TWO), "CONTROL: {two}");
    let refused = render_text(&q, &ctx(&w, &s, None, false));
    let only: Vec<&str> = refused.lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(only.len(), 1, "{only:#?}");
    assert!(
        only[0].starts_with("held unavailable(refused) "),
        "{only:#?}"
    );
    let refused = render_json(&jq, &ctx(&w, &s, None, false));
    let held = refused.get("disk").and_then(|d| d.get("held"));
    assert_eq!(held, Some(&Jv::why("refused")));
}

/// WHAT THE PASSES REMOVED SHOWS IN SYS DISK HELD (DC-05): each deed of the disk journal that
/// removed something is a `removed` line, newest first, with its age, its rows, its bytes,
/// whose it was and what went, and the JSON's `removed` array says the same; a cache folder
/// outside aterm's names the outside-cache step and its days. Nothing of it warns or asks.
/// NEGATIVE CONTROLS: a deed that only checked the disk or was refused is no line, and a
/// snapshot with no disk facts prints none.
#[test]
fn sys_disk_held_says_what_the_passes_removed_and_the_outside_cache_step() {
    use crate::model::{DeedBy, DeedKind, DiskDeed, DiskFacts};
    let outside = Next::OutsideCaches { idle_days: 7 };
    let caches = Holding {
        worktree: Worktree::Caches,
        class: Class::Rebuildable,
        next: outside,
        ..row("/Users//a/.cache", Bytes::AtLeast(90))
    };
    let w = world(vec![caches], 1_000);
    let now = 1_792_185_697_412_i64;
    let deed = |ago_ms: i64, kind: DeedKind, bytes: Option<u64>| DiskDeed {
        at_unix_ms: now - ago_ms,
        n: 2,
        kind,
        by: DeedBy::Aterm,
        bytes,
        free: None,
    };
    let mut s = snap();
    s.facts.disk = Some(DiskFacts {
        deeds: vec![
            deed(
                60_000,
                DeedKind::Removed("Caches/tool-x".into()),
                Some(5 << 20),
            ),
            deed(120_000, DeedKind::Checked, None),
            deed(180_000, DeedKind::Refused("x".into()), None),
            DiskDeed {
                by: DeedBy::Tab(Some(2)),
                ..deed(3_600_000, DeedKind::Removed("build caches".into()), None)
            },
        ],
        ..DiskFacts::default()
    });
    let c = ctx(&w, &s, None, true);
    let r = render_text(&SysQuery::parse("disk held").unwrap(), &c);
    says(&r, 1, "none", None, "rebuildable", outside);
    assert_eq!(
        line_of(&r, "removed 1"),
        "removed n=1 age_s=60 rows=2 bytes=5242880 by=aterm what=Caches/tool-x"
    );
    assert_eq!(
        line_of(&r, "removed 2"),
        "removed n=2 age_s=3600 rows=2 bytes=unavailable(not_read) by=tab:2 what=build%20caches"
    );
    assert!(
        !r.lines.iter().any(|l| l.key == "removed 3"),
        "{:#?}",
        r.lines
    );
    for l in &r.lines {
        assert!(!asks_or_warns(&l.text), "{}", l.text);
    }
    let json = render_json(&SysQuery::parse("disk held --json").unwrap(), &c);
    let violations = contract_violations(&json);
    assert!(violations.is_empty(), "{violations:?}");
    let held = json.get("disk").and_then(|d| d.get("held")).expect("held");
    let Some(Jv::Arr(removed)) = held.get("removed") else {
        panic!("no removed array: {}", json.to_text());
    };
    assert_eq!(removed.len(), 2, "{}", json.to_text());
    assert_eq!(removed[0].get("what"), Some(&Jv::s("Caches/tool-x")));
    assert_eq!(removed[1].get("by"), Some(&Jv::s("tab:2")));
    let next = json_rows(&json)[0].get("next").cloned().expect("next");
    assert_eq!(next.get("pass"), Some(&Jv::s("outside-caches")));
    assert_eq!(next.get("idle_days"), Some(&Jv::u(7)));

    let bare = snap();
    let c = ctx(&w, &bare, None, true);
    let r = render_text(&SysQuery::parse("disk held").unwrap(), &c);
    assert!(
        !r.lines.iter().any(|l| l.key.starts_with("removed")),
        "CONTROL: {:#?}",
        r.lines
    );
}
