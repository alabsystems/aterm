// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The entry-bound pass (DC-19): a build directory whose `deps/` holds the entry bound or more
//! is emptied at any `disk=` level where tier 3 may take it, a build cache over the bound gets
//! its own sweep, and a directory that stays gets one `entry-bound` row. Each test pretends a
//! small bound on its thread: 8 entries (clear count 6, step 1), and 40 (clear count 30, step
//! 4) where a step must be more than one entry; the shipped one is 100,000.

use super::tests::{Tmp, lay_profile, lay_target, now_after_write};
use super::*;
use crate::harness::cli;
use aterm_uds::holders::PathScan;

/// The pretended bound.
const BOUND: u32 = 8;

/// The pretended bound of the test that reads steps: its clear count is 30 and its step 4.
const WIDE: u32 = 40;

/// The free space every look reads: the volume is `ok`, far above any floor.
const FREE: u64 = 500 * GIB;

/// The cache sweep's words for a registered build cache, as `fences` says them.
const MANAGED: &str = "registered build-cache policy owns retirement";

/// What a row's text says where the look ran its build cache's own sweep (its `swept` field).
const SWEPT: &str = "its build cache's own sweep ran at this look";

/// A pretended entry bound on this thread, the shipped one again once it is dropped.
struct Pretend;

impl Pretend {
    fn of(bound: u32) -> Pretend {
        target::pretend_entry_bound(Some(bound));
        Pretend
    }
}

impl Drop for Pretend {
    fn drop(&mut self) {
        target::pretend_entry_bound(None);
    }
}

/// No process uses anything.
fn nobody(_: &[PathBuf], _: &[u32]) -> std::io::Result<PathScan> {
    Ok(PathScan::default())
}

/// A build cache's own sweep that no look here may run.
fn no_sweep(_: &mut cli::Take<'_>) -> Option<u64> {
    panic!("no build cache is over the bound")
}

/// The pass lock is this look's.
fn ours() -> Result<(), String> {
    Ok(())
}

/// The pass lock is another's.
fn held() -> Result<(), String> {
    Err("held".to_owned())
}

/// How many entries the directory at `dir` holds, listed.
fn listed(dir: &Path) -> usize {
    std::fs::read_dir(dir).expect("listed").count()
}

/// Fill `profile`'s `deps/` up to `entries` entries, each a new name.
fn fill_deps(profile: &Path, entries: u32) {
    let deps = profile.join("deps");
    let want = usize::try_from(entries).expect("a small count");
    let mut i = 0;
    while listed(&deps) < want {
        let dep = deps.join(format!("libfill-{i}.rlib"));
        if !dep.exists() {
            std::fs::write(&dep, b"rlib").expect("a dep");
        }
        i += 1;
    }
}

/// Take files out of `profile`'s `deps/` until it holds `entries` entries.
fn trim_deps(profile: &Path, entries: u32) {
    let want = usize::try_from(entries).expect("a small count");
    let mut deps: Vec<PathBuf> = std::fs::read_dir(profile.join("deps"))
        .expect("listed")
        .map(|e| e.expect("an entry").path())
        .collect();
    deps.sort();
    while deps.len() > want {
        std::fs::remove_file(deps.pop().expect("a dep")).expect("trimmed");
    }
}

/// A checkout at `repo` that builds into `target.noindex` behind `target -> target.noindex`
/// (discovery finds it there), its `deps/` holding `entries`: its canonical root and build
/// directory.
fn linked_checkout(repo: &Path, entries: u32) -> (PathBuf, PathBuf) {
    std::fs::create_dir_all(repo.join(".git/logs")).expect("git");
    std::fs::write(repo.join(".git/index"), b"DIRC").expect("index");
    std::fs::write(repo.join(".git/logs/HEAD"), b"0 1 a\n").expect("head");
    std::fs::create_dir_all(repo.join("src")).expect("src");
    std::fs::write(repo.join("Cargo.toml"), b"[package]\n").expect("manifest");
    std::fs::write(repo.join("src/main.rs"), b"fn main() {}\n").expect("source");
    let noindex = lay_target(&repo.join("target.noindex"));
    std::os::unix::fs::symlink("target.noindex", repo.join("target")).expect("target link");
    fill_deps(&noindex.join("debug"), entries);
    let canon = |p: &Path| std::fs::canonicalize(p).expect("canonical");
    (canon(repo), canon(&noindex))
}

/// A build directory cargo laid at `dir`, its `deps/` holding `entries`: its canonical path.
fn laid(dir: &Path, entries: u32) -> PathBuf {
    lay_target(dir);
    fill_deps(&dir.join("debug"), entries);
    std::fs::canonicalize(dir).expect("canonical")
}

/// What discovery searches under `tmp`: its `home/` and Claude Code's temp root, nothing else.
fn found_under(tmp: &Tmp) -> discover::Roots {
    let home = tmp.path().join("home");
    discover::Roots {
        claude_dirs: vec![home.join(".claude")],
        home: Some(home),
        claude_tmp: Some(tmp.path().join("claude-tmp")),
        ..discover::Roots::default()
    }
}

/// A look at `now` over the named `targets`, or with none over what `found` discovers, the
/// volume `ok` at [`FREE`].
fn look<'a>(
    state: &'a Path,
    volume: &'a Path,
    targets: &'a [PathBuf],
    found: Option<&'a discover::Roots>,
    table: &'a aterm_sysprobe::volumes::Table,
    now: i64,
) -> cli::DiskLook<'a> {
    cli::DiskLook {
        state,
        volume,
        free: Some(FREE),
        targets,
        discover: found,
        transcripts: None,
        now,
        room: None,
        table: Some(table),
        demand: None,
        projected: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    }
}

/// One look of the entry-bound pass: `live` as the live sessions, no process, `take` as the
/// pass lock, the real remover judging nobody's sessions and processes, and `sweep` as the
/// build cache's own.
fn bound_tick_with(
    look: &cli::DiskLook<'_>,
    config: Config,
    live: &budget::Live,
    take: &mut cli::Take<'_>,
    sweep: &mut dyn FnMut(&mut cli::Take<'_>) -> Option<u64>,
) -> cli::BoundTick {
    let judge = Judge {
        now: look.now,
        threshold_days: config.target_stale_days,
        device: None,
        volume: None,
    };
    let mut remove = remover_among(
        judge,
        None,
        nobody,
        |_| Ok(discover::Running::default()),
        idle::Sessions::default,
        aterm_sysprobe::volumes::table,
    );
    let bound = cli::BoundLook {
        live,
        processes: &nobody,
    };
    cli::disk_tick_bound(
        look,
        config,
        &bound,
        take,
        &mut remove,
        &mut discover::Cursor::new(),
        sweep,
    )
}

/// [`bound_tick_with`] with no live session and the pass lock its own.
fn bound_tick(
    look: &cli::DiskLook<'_>,
    config: Config,
    sweep: &mut dyn FnMut(&mut cli::Take<'_>) -> Option<u64>,
) -> cli::BoundTick {
    bound_tick_with(look, config, &budget::Live::default(), &mut ours, sweep)
}

/// Every row of `state`'s disk ledger.
fn ledger(state: &Path) -> Vec<Value> {
    std::fs::read_to_string(state.join(cli::DISK_LEDGER))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| aterm_json::from_str::<Value>(line).ok())
        .collect()
}

/// A ledger row's kind.
fn kind(row: &Value) -> &str {
    row.get("kind").and_then(Value::as_str).unwrap_or("")
}

/// The `entry-bound` rows of `state`'s disk ledger.
fn bound_rows(state: &Path) -> Vec<Value> {
    ledger(state)
        .into_iter()
        .filter(|row| kind(row) == ENTRY_BOUND_ROW_KIND)
        .collect()
}

/// A row's text field.
fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Whether a row says its build cache's own sweep ran: its `swept` field and its text agree.
fn swept_said(row: &Value) -> bool {
    let swept = row
        .get("swept")
        .and_then(Value::as_bool)
        .expect("a swept field");
    assert_eq!(swept, text(row, "text").contains(SWEPT), "{row:?}");
    swept
}

/// The count a row states.
fn entries(row: &Value) -> Option<u64> {
    row.get("entries").and_then(Value::as_u64)
}

/// AN IDLE BUILD DIRECTORY OVER THE ENTRY BOUND IS TAKEN WHILE THE VOLUME READS OK. A checkout
/// at `home/proj` builds into `target.noindex` behind `target -> target.noindex`, found by
/// discovery and idle eight days, its `deps/` at the bound, with 500 GiB free: one look of the
/// entry-bound pass empties its profile, keeps the build directory's markers and the checkout,
/// and journals the removal before and after it. Of two more such checkouts each look then
/// takes one. NEGATIVE CONTROLS, each the mechanism taken away: one entry under the bound, the
/// look finds nothing over it and writes nothing; the pressure tick alone at the same free
/// space takes nothing (no pass but the entry-bound one is admitted at `ok`); with
/// `auto_free_gib = 0` the look finds nothing and writes nothing; a survey that is not an
/// entry-bound look counts no `deps/`; a look whose pass lock is another's stands down with
/// every entry in place, no ledger and no record; a look that reads a live session at work in
/// the checkout takes nothing and writes the session's row; and with `idle_build_dirs` off it
/// takes nothing and the row says so. The shipped bound is read before any is pretended.
#[test]
fn an_idle_build_directory_over_the_entry_bound_is_taken_while_the_volume_reads_ok() {
    assert_eq!(target::entry_bound(), (100_000, 75_000, 10_000));
    let _bound = Pretend::of(BOUND);
    let tmp = Tmp::new("bound-taken");
    let state = tmp.path().join("state");
    let (repo, noindex) = linked_checkout(&tmp.path().join("home/proj"), BOUND - 1);
    let deps = noindex.join("debug/deps");
    let now = now_after_write(8);
    let table = aterm_sysprobe::volumes::table();
    let found = found_under(&tmp);
    let config = Config::default();
    let quiet = budget::Live::default();
    let at = |now: i64| look(&state, tmp.path(), &[], Some(&found), &table, now);

    let under = bound_tick(&at(now), config, &mut no_sweep);
    assert_eq!((under.over, under.rows), (0, 0), "{under:?}");
    assert!(under.taken.is_empty(), "{under:?}");
    assert!(!state.join(cli::DISK_LEDGER).exists());
    assert_eq!(listed(&deps), 7);

    fill_deps(&noindex.join("debug"), BOUND);
    let pressure = cli::disk_tick(
        &at(now),
        config,
        &mut |row| panic!("the pressure tick removed {} at ok", row.path.display()),
        &mut || Some(FREE),
    );
    assert_eq!(pressure, cli::DiskTick::Plenty);
    let off = Config {
        auto_free_gib: 0,
        ..config
    };
    let off = bound_tick(&at(now), off, &mut no_sweep);
    assert_eq!((off.over, off.rows), (0, 0), "{off:?}");
    assert!(!state.join(cli::DISK_LEDGER).exists());
    let plain = scan(
        &Roots {
            volume: Some(tmp.path().to_path_buf()),
            targets: vec![noindex.clone()],
            ..Roots::default()
        },
        now,
        Trigger::Tick,
        config,
        Some(FREE),
    );
    assert!(!plain.targets.is_empty());
    assert!(
        plain
            .targets
            .iter()
            .flat_map(|t| &t.profiles)
            .all(|p| p.deps_entries.is_none()),
        "only an entry-bound look counts deps/"
    );

    let refused = bound_tick_with(&at(now), config, &quiet, &mut held, &mut no_sweep);
    assert_eq!(refused.stood_down.as_deref(), Some("held"));
    assert!(refused.taken.is_empty(), "{refused:?}");
    assert!(!state.join(cli::DISK_LEDGER).exists());
    assert!(!state.join(ENTRY_BOUND_FILE).exists());

    let live_state = tmp.path().join("live-state");
    let live = budget::Live {
        cwds: vec![repo.clone()],
        ..budget::Live::default()
    };
    let worked = bound_tick_with(
        &look(&live_state, tmp.path(), &[], Some(&found), &table, now),
        config,
        &live,
        &mut ours,
        &mut no_sweep,
    );
    assert!(worked.taken.is_empty(), "{worked:?}");
    assert_eq!((worked.over, worked.rows), (1, 1), "{worked:?}");
    let rows = bound_rows(&live_state);
    let why = text(&rows[0], "why");
    assert!(why.contains("a session works in"), "{why}");

    let off_state = tmp.path().join("off-state");
    let report_only = Config {
        idle_build_dirs: false,
        ..config
    };
    let kept = bound_tick(
        &look(&off_state, tmp.path(), &[], Some(&found), &table, now),
        report_only,
        &mut no_sweep,
    );
    assert!(kept.taken.is_empty(), "{kept:?}");
    assert_eq!((kept.over, kept.rows), (1, 1), "{kept:?}");
    let rows = bound_rows(&off_state);
    let why = text(&rows[0], "why");
    assert!(why.contains("`[disk] idle_build_dirs` is off"), "{why}");
    assert_eq!(listed(&deps), 8);

    let taken = bound_tick(&at(now), config, &mut no_sweep);
    assert_eq!(taken.taken, vec![noindex.clone()], "{taken:?}");
    assert_eq!(taken.stood_down, None);
    assert_eq!(taken.rows, 0, "one taken under the clear count gets no row");
    assert!(!deps.exists(), "the profile was emptied");
    for kept in [
        noindex.join("CACHEDIR.TAG"),
        noindex.join(".rustc_info.json"),
        noindex.join("debug/.cargo-lock"),
        repo.join("src/main.rs"),
        repo.join("Cargo.toml"),
    ] {
        assert!(kept.exists(), "{} stays", kept.display());
    }
    let kinds: Vec<String> = ledger(&state).iter().map(|r| kind(r).to_owned()).collect();
    let removing = kinds.iter().position(|k| k == "removing");
    let removed = kinds.iter().position(|k| k == "removed");
    assert!(
        removing.is_some() && removing < removed,
        "the removal is journalled before and after: {kinds:?}"
    );
    assert!(BoundMemo::read(&state).folders.is_empty());

    let (_, second) = linked_checkout(&tmp.path().join("home/proj2"), BOUND);
    let (_, third) = linked_checkout(&tmp.path().join("home/proj3"), BOUND);
    let both = [second.join("debug/deps"), third.join("debug/deps")];
    let standing = || both.iter().filter(|deps| deps.exists()).count();
    let one = bound_tick(&at(now), config, &mut no_sweep);
    assert_eq!((one.over, one.rows), (2, 0), "{one:?}");
    assert_eq!(one.taken.len(), 1, "{one:?}");
    assert_eq!(standing(), 1, "one directory a look");
    let other = bound_tick(&at(now), config, &mut no_sweep);
    assert_eq!((other.over, other.rows), (1, 0), "{other:?}");
    assert_eq!(other.taken.len(), 1, "{other:?}");
    assert_eq!(standing(), 0);
    assert_ne!(one.taken, other.taken);
}

/// AN ENTRY-BOUND PASS RUNS THE CACHE'S SWEEP FOR A DIRECTORY THAT STAYS. A build directory in
/// a registered cache's custody (`.aterm-build-cache.json` beside its signed `CACHEDIR.TAG`) is
/// over the bound; tier 3 never takes it, so the look runs the cache's own sweep once, each of
/// its removals behind the look's own pass lock (asked three times for three removals). The
/// sweep takes three entries, the count falls under the clear count, and nothing is written. A
/// leaf of the shared build cache that discovery finds gets the sweep the same way. NEGATIVE
/// CONTROLS: a sweep that takes nothing leaves the directory over the bound, and its one row
/// says the cache's policy keeps it and, in its `swept` field and text but not its reason, that
/// its sweep ran; a day on, a look that runs no sweep writes no second row (the reason did not
/// change); where the host ran no sweep the row does not say one ran; an unregistered build
/// directory the look keeps never runs the sweep.
#[test]
fn an_entry_bound_pass_runs_the_caches_sweep_for_a_directory_that_stays() {
    let _bound = Pretend::of(BOUND);
    let tmp = Tmp::new("bound-sweep");
    let state = tmp.path().join("state");
    let cache = tmp.path().join("cache/ws");
    lay_target(&cache);
    std::fs::write(cache.join(atpkg::build_cache::registered::MARKER), b"{}").expect("custody");
    fill_deps(&cache.join("debug"), BOUND);
    let cache = std::fs::canonicalize(&cache).expect("canonical");
    let deps = cache.join("debug/deps");
    let now = now_after_write(8);
    let table = aterm_sysprobe::volumes::table();
    let config = Config::default();
    let quiet = budget::Live::default();
    let named = [cache.clone()];
    let at = look(&state, tmp.path(), &named, None, &table, now);

    let mut takes = 0u32;
    let mut counted = || -> Result<(), String> {
        takes += 1;
        Ok(())
    };
    let mut calls = 0u32;
    let swept = bound_tick_with(&at, config, &quiet, &mut counted, &mut |take| {
        calls += 1;
        let mut fills: Vec<PathBuf> = std::fs::read_dir(&deps)
            .expect("deps")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains("libfill-"))
            .collect();
        fills.sort();
        let mut freed = 0;
        for dep in fills.into_iter().take(3) {
            if take().is_ok() {
                std::fs::remove_file(dep).expect("swept");
                freed += 4;
            }
        }
        Some(freed)
    });
    assert_eq!(calls, 1, "{swept:?}");
    assert_eq!(takes, 3, "the sweep asks the look's pass lock");
    assert_eq!((swept.over, swept.rows), (1, 0), "{swept:?}");
    assert_eq!(swept.swept, 12, "{swept:?}");
    assert!(swept.taken.is_empty(), "{swept:?}");
    assert_eq!(listed(&deps), 5);
    assert!(BoundMemo::read(&state).folders.is_empty());
    assert!(!state.join(cli::DISK_LEDGER).exists());

    fill_deps(&cache.join("debug"), BOUND);
    let mut calls = 0u32;
    let stays = bound_tick(&at, config, &mut |_| {
        calls += 1;
        Some(0)
    });
    assert_eq!((calls, stays.rows), (1, 1), "{stays:?}");
    let rows = bound_rows(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let why = text(&rows[0], "why");
    assert!(why.contains(MANAGED), "{why}");
    assert!(
        !why.contains(SWEPT),
        "the sweep is no part of the reason: {why}"
    );
    assert!(swept_said(&rows[0]), "{:?}", rows[0]);
    assert_eq!(text(&rows[0], "path"), cache.display().to_string());
    // A day on the directory is assessed again and this look runs no sweep: the reason is the
    // same, so no row is written for the sweep's absence.
    let mut calls = 0u32;
    let later = bound_tick(
        &look(&state, tmp.path(), &named, None, &table, now + 25 * 3_600),
        config,
        &mut |_| {
            calls += 1;
            None
        },
    );
    assert_eq!((later.over, later.rows, calls), (1, 0, 1), "{later:?}");
    assert_eq!(bound_rows(&state).len(), 1);

    let unswept_state = tmp.path().join("unswept-state");
    let mut calls = 0u32;
    let unswept = bound_tick(
        &look(&unswept_state, tmp.path(), &named, None, &table, now),
        config,
        &mut |_| {
            calls += 1;
            None
        },
    );
    assert_eq!((calls, unswept.rows), (1, 1), "{unswept:?}");
    let rows = bound_rows(&unswept_state);
    let why = text(&rows[0], "why");
    assert!(why.contains(MANAGED), "{why}");
    assert!(!swept_said(&rows[0]), "{:?}", rows[0]);

    let other_state = tmp.path().join("other-state");
    let plain = laid(&tmp.path().join("plain/target"), BOUND);
    let named = [plain];
    let mut calls = 0u32;
    let kept = bound_tick(
        &look(&other_state, tmp.path(), &named, None, &table, now),
        config,
        &mut |_| {
            calls += 1;
            Some(0)
        },
    );
    assert_eq!((kept.over, kept.rows, calls), (1, 1, 0), "{kept:?}");

    let home = tmp.path().join("home");
    let shared = atpkg::build_cache::cache_root(&home).join("Users/a/proj/.cargo-build");
    let shared = laid(&shared, BOUND);
    let found = found_under(&tmp);
    let shared_state = tmp.path().join("shared-state");
    let mut calls = 0u32;
    let leaf = bound_tick(
        &look(&shared_state, tmp.path(), &[], Some(&found), &table, now),
        config,
        &mut |_| {
            calls += 1;
            Some(0)
        },
    );
    assert_eq!((leaf.over, leaf.rows, calls), (1, 1, 1), "{leaf:?}");
    let rows = bound_rows(&shared_state);
    assert_eq!(text(&rows[0], "path"), shared.display().to_string());
    let why = text(&rows[0], "why");
    assert!(why.contains("a build cache holds it"), "{why}");
    assert!(swept_said(&rows[0]), "{:?}", rows[0]);
}

/// A BUILD DIRECTORY OVER THE ENTRY BOUND THAT A FENCE KEEPS GETS ONE ENTRY-BOUND ROW. Test 1's
/// checkout, written an hour before the look, is inside tier 3's idle window, under a bound of
/// 40 (clear count 30, step 4): the look keeps it and writes one `entry-bound` row with its
/// count and the window clause, and no other row. A look an hour later finds it settled and
/// writes nothing; one a day later assesses it again, its age now `1d`, the same reason, and
/// writes nothing. Its `deps/` keeps every entry. One more entry is no step: it stays settled
/// within the day, and a day on it is assessed and writes nothing. Four entries past the row's
/// count are a step and write a second row, measured from the count the row stated and not
/// from the last look's. Down at 35, between the clear count and the bound, it is still over
/// the bound and its row is written again; a session at work in the checkout changes the
/// reason and writes a row at the same count; under the clear count it leaves the record.
/// NEGATIVE CONTROLS: at one entry under the bound the first look writes nothing; a first look
/// whose pass lock is another's writes no row and no record; and a directory at 35 that no
/// look found over the bound is not over it.
#[test]
fn a_build_directory_over_the_entry_bound_that_a_fence_keeps_gets_one_entry_bound_row() {
    let _bound = Pretend::of(WIDE);
    let tmp = Tmp::new("bound-kept");
    let state = tmp.path().join("state");
    let (repo, noindex) = linked_checkout(&tmp.path().join("home/proj"), WIDE - 1);
    let debug = noindex.join("debug");
    let deps = debug.join("deps");
    let now = now_after_write(0) + 3_600;
    let table = aterm_sysprobe::volumes::table();
    let found = found_under(&tmp);
    let config = Config::default();
    let quiet = budget::Live::default();
    let at = |now: i64| look(&state, tmp.path(), &[], Some(&found), &table, now);

    let under = bound_tick(&at(now), config, &mut no_sweep);
    assert_eq!((under.over, under.rows), (0, 0), "{under:?}");
    assert!(!state.join(cli::DISK_LEDGER).exists());

    fill_deps(&debug, WIDE);
    let refused = bound_tick_with(&at(now), config, &quiet, &mut held, &mut no_sweep);
    assert_eq!(refused.stood_down.as_deref(), Some("held"));
    assert_eq!((refused.over, refused.rows), (1, 0), "{refused:?}");
    assert!(!state.join(cli::DISK_LEDGER).exists());
    assert!(!state.join(ENTRY_BOUND_FILE).exists());

    let first = bound_tick(&at(now), config, &mut no_sweep);
    assert_eq!((first.over, first.rows), (1, 1), "{first:?}");
    let settled = bound_tick(&at(now + 3_600), config, &mut no_sweep);
    assert_eq!((settled.over, settled.rows), (0, 0), "{settled:?}");
    let again = bound_tick(&at(now + 25 * 3_600), config, &mut no_sweep);
    assert_eq!((again.over, again.rows), (1, 0), "{again:?}");
    assert!(
        BoundMemo::read(&state)
            .folders
            .iter()
            .any(|f| f.why.contains("1d ago")),
        "the record carries the look's own words"
    );

    let rows = ledger(&state);
    assert!(
        rows.iter().all(|r| kind(r) == ENTRY_BOUND_ROW_KIND),
        "only entry-bound rows: {rows:?}"
    );
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(entries(&rows[0]), Some(40));
    let why = text(&rows[0], "why");
    assert!(why.contains("inside the"), "{why}");
    let said = text(&rows[0], "text")
        .replace(&deps.display().to_string(), "")
        .to_lowercase();
    for word in ["targo clean", "remove", "you"] {
        assert!(!said.contains(word), "{said}");
    }
    assert_eq!(listed(&deps), 40, "nothing in it was removed");

    fill_deps(&debug, WIDE + 1);
    let drift = bound_tick(&at(now + 25 * 3_600), config, &mut no_sweep);
    assert_eq!((drift.over, drift.rows), (0, 0), "{drift:?}");
    let drift = bound_tick(&at(now + 50 * 3_600), config, &mut no_sweep);
    assert_eq!((drift.over, drift.rows), (1, 0), "{drift:?}");

    fill_deps(&debug, WIDE + 4);
    let stepped = bound_tick(&at(now + 50 * 3_600), config, &mut no_sweep);
    assert_eq!((stepped.over, stepped.rows), (1, 1), "{stepped:?}");
    let rows = bound_rows(&state);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(entries(&rows[1]), Some(44));

    trim_deps(&debug, 35);
    let lower = bound_tick(&at(now + 50 * 3_600), config, &mut no_sweep);
    assert_eq!((lower.over, lower.rows), (1, 1), "{lower:?}");
    let rows = bound_rows(&state);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(entries(&rows[2]), Some(35));
    assert!(BoundMemo::read(&state).of(&noindex).is_some());
    let fresh_state = tmp.path().join("fresh-state");
    let unraised = bound_tick(
        &look(&fresh_state, tmp.path(), &[], Some(&found), &table, now),
        config,
        &mut no_sweep,
    );
    assert_eq!((unraised.over, unraised.rows), (0, 0), "{unraised:?}");

    let cwds = [repo];
    let worked = cli::DiskLook {
        session_cwds: &cwds,
        ..at(now + 9 * 86_400)
    };
    let session = bound_tick(&worked, config, &mut no_sweep);
    assert_eq!((session.over, session.rows), (1, 1), "{session:?}");
    let rows = bound_rows(&state);
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert_eq!(entries(&rows[3]), Some(35));
    let why = text(&rows[3], "why");
    assert!(why.contains("a session works in"), "{why}");
    assert!(!why.contains("inside the"), "{why}");

    trim_deps(&debug, 29);
    let cleared = bound_tick(&worked, config, &mut no_sweep);
    assert_eq!((cleared.over, cleared.rows), (0, 0), "{cleared:?}");
    assert!(BoundMemo::read(&state).folders.is_empty());
    assert_eq!(bound_rows(&state).len(), 4);
}

/// A FOLDER OVER THE ENTRY BOUND THAT NO BUILD TOOL MARKED IS NEVER PLANNED. A folder with a
/// cargo profile but neither a signed `CACHEDIR.TAG` nor `.rustc_info.json`, and a marked build
/// directory that holds `Cargo.toml` at its root, each over the bound: the look finds neither
/// over it, plans nothing, writes nothing, runs no sweep and leaves every entry. NEGATIVE
/// CONTROL: a marked build directory with no source beside them is found, and its one row says
/// tier 3 takes only a checkout's build directory.
#[test]
fn a_folder_over_the_entry_bound_that_no_build_tool_marked_is_never_planned() {
    let _bound = Pretend::of(BOUND);
    let tmp = Tmp::new("bound-unmarked");
    let state = tmp.path().join("state");
    let plain = tmp.path().join("plain/out");
    lay_profile(&plain.join("debug"));
    fill_deps(&plain.join("debug"), BOUND);
    let plain = std::fs::canonicalize(&plain).expect("canonical");
    let source = tmp.path().join("source");
    lay_target(&source);
    std::fs::write(source.join("Cargo.toml"), b"[package]\n").expect("manifest");
    fill_deps(&source.join("debug"), BOUND);
    let source = std::fs::canonicalize(&source).expect("canonical");
    let now = now_after_write(8);
    let table = aterm_sysprobe::volumes::table();
    let mut calls = 0u32;

    let named = [plain.clone(), source.clone()];
    let none = bound_tick(
        &look(&state, tmp.path(), &named, None, &table, now),
        Config::default(),
        &mut |_| {
            calls += 1;
            Some(0)
        },
    );
    assert_eq!((none.over, none.rows), (0, 0), "{none:?}");
    assert!(none.taken.is_empty());
    assert!(!state.join(cli::DISK_LEDGER).exists());
    assert_eq!(listed(&plain.join("debug/deps")), 8);
    assert_eq!(listed(&source.join("debug/deps")), 8);
    assert!(source.join("Cargo.toml").exists());

    let marked = laid(&tmp.path().join("marked/target"), BOUND);
    let named = [plain.clone(), source.clone(), marked.clone()];
    let one = bound_tick(
        &look(&state, tmp.path(), &named, None, &table, now),
        Config::default(),
        &mut |_| {
            calls += 1;
            Some(0)
        },
    );
    assert_eq!((one.over, one.rows), (1, 1), "{one:?}");
    let rows = ledger(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(kind(&rows[0]), ENTRY_BOUND_ROW_KIND);
    assert_eq!(text(&rows[0], "path"), marked.display().to_string());
    assert!(
        text(&rows[0], "why").contains("tier 3 takes only a git checkout's build directory"),
        "{rows:?}"
    );
    assert_eq!(calls, 0, "no build cache is over the bound");
    assert_eq!(listed(&plain.join("debug/deps")), 8);
    assert_eq!(listed(&source.join("debug/deps")), 8);
}

/// THE RECORD PUTS NO DIRECTORY OUT FOR ANOTHER. A record that holds as many build directories
/// as it keeps takes no new one and loses none; one it holds is replaced where it stands; the
/// full record reads back whole from its file; and a directory whose path is not text the file
/// gives back unchanged is not taken. NEGATIVE CONTROL: put the oldest out for the newcomer and
/// the first directory is gone from a full record, where each of the two would then get a row
/// at every look.
#[test]
fn the_entry_bound_record_puts_no_directory_out_for_another() {
    use std::os::unix::ffi::OsStrExt as _;
    let entry = |dir: &Path, entries: u32| BoundEntry {
        dir: dir.to_path_buf(),
        deps: dir.join("debug/deps"),
        entries,
        why: "kept".to_owned(),
        at: 1,
    };
    let dir = |i: usize| PathBuf::from(format!("/build/{i}"));
    let mut memo = BoundMemo::default();
    for i in 0..ENTRY_BOUND_KEPT {
        assert!(memo.set(&dir(i), Some(entry(&dir(i), 1))), "{i}");
    }
    let late = PathBuf::from("/build/late");
    assert!(!memo.set(&late, Some(entry(&late, 1))), "a full record");
    assert_eq!(memo.folders.len(), ENTRY_BOUND_KEPT);
    assert_eq!(memo.folders[0].dir, dir(0), "none is put out");
    assert!(memo.set(&dir(0), Some(entry(&dir(0), 9))));
    assert_eq!(memo.folders[0].entries, 9, "replaced where it stands");
    let tmp = Tmp::new("bound-record");
    memo.save(tmp.path()).expect("saved");
    assert_eq!(BoundMemo::read(tmp.path()), memo, "read back whole");
    assert!(!memo.set(&dir(0), None));
    assert!(memo.set(&late, Some(entry(&late, 1))), "room again");
    let odd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/build/\xff"));
    let mut small = BoundMemo::default();
    assert!(!small.set(&odd, Some(entry(&odd, 1))), "no text path");
    assert!(small.folders.is_empty());
}

/// A CUT PROFILE WALK STILL COUNTS THE USUAL PROFILE FOLDERS. A build directory whose profile
/// walk was cut short before it listed `debug/` carries no profile; an entry-bound look still
/// reads `debug/deps`'s count, so the directory is over the bound and gets its row. NEGATIVE
/// CONTROL: with no profile and a walk that was not cut, no `deps/` is counted.
#[test]
fn a_cut_profile_walk_still_counts_the_usual_profile_folders() {
    let tmp = Tmp::new("bound-cut-walk");
    let dir = laid(&tmp.path().join("ws/target"), BOUND);
    let deps = dir.join("debug/deps");
    let (now, stale) = (now_after_write(0), DEFAULT_TARGET_STALE_DAYS);
    let mut td = scan_target(&dir, now, stale, false).expect("a build directory");
    td.count_deps();
    assert_eq!(td.deps_peak(), Some((deps.clone(), BOUND)));
    assert_eq!(td.deps_unwalked, None);
    td.profiles.clear();
    td.count_deps();
    assert_eq!(td.deps_peak(), None, "a whole walk found no profile");
    td.walk_cut_short = Some("the walk ran out of entries".to_owned());
    td.count_deps();
    assert_eq!(td.deps_peak(), Some((deps, BOUND)));
}

/// AN ENTRY-BOUND LOOK SIZES NO DIRECTORY A RECENT BUILD OR A SESSION KEEPS. Test 1's checkout,
/// over the bound: written an hour before the look, the survey carries tier 3's window clause
/// for it and no size; idle eight days with a live session's working directory in the checkout,
/// the session's clause and no size. Neither was walked, so a directory a person builds in
/// costs an hourly look its profile clocks and its count alone. NEGATIVE CONTROL: idle eight
/// days with no session, it is sized and assessed idle, as before.
#[test]
fn an_entry_bound_look_sizes_no_directory_a_recent_build_or_a_session_keeps() {
    let _bound = Pretend::of(BOUND);
    let tmp = Tmp::new("bound-unsized");
    let (repo, noindex) = linked_checkout(&tmp.path().join("home/proj"), BOUND);
    let table = aterm_sysprobe::volumes::table();
    let found = found_under(&tmp);
    let survey_at = |now: i64, cwds: Vec<PathBuf>| {
        let roots = Roots {
            volume: Some(tmp.path().to_path_buf()),
            found: discover::discover(&found, discover::Budget::SHIPPED).found,
            home: found.home.clone(),
            mounts: Some(table.clone()),
            session_cwds: cwds,
            entry_bound: Some(EntryBound::default()),
            ..Roots::default()
        };
        let survey = scan_with(
            &roots,
            now,
            Trigger::Tick,
            Config::default(),
            Some(FREE),
            &nobody,
        );
        survey
            .targets
            .into_iter()
            .find(|t| t.path == noindex)
            .expect("the build directory is found")
    };
    let recent = survey_at(now_after_write(0) + 3_600, Vec::new());
    assert_eq!(
        recent.total, None,
        "a recent build's directory is not sized"
    );
    assert!(
        matches!(&recent.idle, Some(Err(why)) if why.contains("inside the")),
        "{:?}",
        recent.idle
    );
    let idle = now_after_write(8);
    let worked = survey_at(idle, vec![repo]);
    assert_eq!(worked.total, None, "a session's directory is not sized");
    assert!(
        matches!(&worked.idle, Some(Err(why)) if why.contains("a session works in")),
        "{:?}",
        worked.idle
    );
    let quiet = survey_at(idle, Vec::new());
    assert!(
        quiet.total.is_some(),
        "an idle directory nobody works in is sized"
    );
    assert!(matches!(quiet.idle, Some(Ok(_))), "{:?}", quiet.idle);
}

/// A COUNT THAT COULD NOT BE READ KEEPS THE RECORD'S ENTRY. The record holds two build
/// directories no look reaches: one whose `deps/` cannot be opened (no permission) and one
/// whose `deps/` is gone. A look keeps the first as the record said it and lets the second go,
/// so a passing read error neither ends a directory's raised state nor writes its row again.
/// NEGATIVE CONTROL: once the first `deps/` reads again, under the clear count, it leaves the
/// record too.
#[test]
fn a_count_that_could_not_be_read_keeps_the_records_entry() {
    use std::os::unix::fs::PermissionsExt as _;
    let _bound = Pretend::of(BOUND);
    let tmp = Tmp::new("bound-unread");
    let state = tmp.path().join("state");
    let shut = laid(&tmp.path().join("shut/target"), 2);
    let gone = laid(&tmp.path().join("gone/target"), 2);
    std::fs::remove_dir_all(gone.join("debug/deps")).expect("gone");
    let entry = |dir: &Path| BoundEntry {
        dir: dir.to_path_buf(),
        deps: dir.join("debug/deps"),
        entries: BOUND,
        why: "kept".to_owned(),
        at: 1,
    };
    BoundMemo {
        folders: vec![entry(&shut), entry(&gone)],
    }
    .save(&state)
    .expect("a record");
    let deps = shut.join("debug/deps");
    std::fs::set_permissions(&deps, std::fs::Permissions::from_mode(0o000)).expect("shut");
    let unreadable = target::deps_entries_of(&deps);
    let table = aterm_sysprobe::volumes::table();
    let now = now_after_write(0);
    let tick = bound_tick(
        &look(&state, tmp.path(), &[], None, &table, now),
        Config::default(),
        &mut no_sweep,
    );
    std::fs::set_permissions(&deps, std::fs::Permissions::from_mode(0o755)).expect("opened");
    let err = unreadable.expect_err("a shut deps/ cannot be counted");
    assert_ne!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
    assert_eq!((tick.over, tick.rows), (0, 0), "{tick:?}");
    let kept: Vec<PathBuf> = BoundMemo::read(&state)
        .folders
        .into_iter()
        .map(|f| f.dir)
        .collect();
    assert_eq!(
        kept,
        vec![shut.clone()],
        "the unread one stays, the gone one goes"
    );
    let opened = bound_tick(
        &look(&state, tmp.path(), &[], None, &table, now),
        Config::default(),
        &mut no_sweep,
    );
    assert_eq!(opened.rows, 0, "{opened:?}");
    assert!(BoundMemo::read(&state).folders.is_empty());
}
