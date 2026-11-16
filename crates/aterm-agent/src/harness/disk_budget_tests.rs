// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the rebuildable budget: `disk.cap_used_gib`, which looks may size it, what a
//! sizing counts, the record every process shares, and that no level reads any of it.

use super::*;
use crate::harness::cli;
#[cfg(unix)]
use crate::harness::disk::tests::now_after_write;
use crate::harness::disk::tests::{Tmp, lay_target};
#[cfg(unix)]
use crate::harness::disk::{
    CANNOT_MEET_ROW_KIND, Class, Judge, Removed, Roots, Row, Step, Trigger, Witness,
};
use crate::harness::disk::{Config, DEFAULT_AUTO_FREE_GIB, Refusal, Written};

const GIB: u64 = 1 << 30;

/// A VOLUME OVER ITS BUDGET STILL READS `disk=ok`. A 926 GiB volume with 147 GiB free and 381
/// GiB rebuildable is 313 GiB over the target a 466 GiB cap leaves, and its thresholds, its
/// knobs in force and its level are those of the same volume with no cap written: the cap is
/// read by [`Config::budget`] alone. CONTROL: down a ladder of free figures both configs read
/// `ok`, `low`, `short`, `emergency`, so the level is not a constant.
#[test]
fn a_volume_over_its_budget_still_reads_disk_ok() {
    let capped = Config {
        cap_used_gib: Cap::Written(466),
        ..Config::default()
    };
    let plain = Config::default();
    let (total, free, rebuildable) = (926 * GIB, 147 * GIB, 381 * GIB);
    let room = Some(64 * GIB);

    let budget = capped
        .budget(total, total - free, rebuildable)
        .expect("a written cap gives a budget");
    assert_eq!((budget.cap, budget.cap_written), (466 * GIB, true));
    assert_eq!((budget.over(), budget.under()), (313 * GIB, 0));

    let lines = capped.thresholds_with(Some(total), Written::default(), room);
    assert_eq!(
        lines,
        plain.thresholds_with(Some(total), Written::default(), room),
        "the cap moves no line"
    );
    let in_force = capped.on_volume_with(Some(total), Written::default(), room);
    assert_eq!(
        in_force.removal_view(),
        plain.on_volume_with(Some(total), Written::default(), room),
        "the cap moves no knob in force"
    );
    assert!(!in_force.below_warn(Some(free)));
    assert!(!in_force.pressed(Some(free), None));

    let level = lines.level(Some(free), None);
    assert_eq!(level, Level::Ok);
    assert_eq!(level.as_str(), "ok");

    let least = Config {
        cap_used_gib: Cap::Written(1),
        ..Config::default()
    };
    let nothing = least
        .budget(total, total - free, rebuildable)
        .expect("a cap of 1 GiB is read as written");
    assert_eq!((nothing.target, nothing.over()), (0, rebuildable));
    assert_eq!(
        least
            .thresholds_with(Some(total), Written::default(), room)
            .level(Some(free), None),
        Level::Ok,
        "every rebuildable byte over, and still ok"
    );

    // CONTROL: the level follows free space, with the cap written or not.
    assert!(lines.warn > lines.floor && lines.floor > lines.emergency && lines.emergency > 0);
    let ladder = [
        lines.warn,
        lines.warn - 1,
        lines.floor - 1,
        lines.emergency - 1,
    ];
    for config in [capped, plain, least] {
        let lines = config.thresholds_with(Some(total), Written::default(), room);
        let words: Vec<&str> = ladder
            .iter()
            .map(|free| lines.level(Some(*free), None).as_str())
            .collect();
        assert_eq!(words, ["ok", "low", "short", "emergency"]);
    }
}

/// `disk.cap_used_gib` is read as written; `0` turns the budget off, as `auto_free_gib = 0`
/// turns removal off; a value that is no whole number turns the budget off and no removal; a
/// negative one is unwritten; an aterm.toml that cannot be read turns the budget off with
/// everything else. CONTROL: the same wrong type on a removal key turns every removal off.
#[test]
fn cap_used_gib_is_read_as_written_and_a_wrong_type_turns_only_the_budget_off() {
    let tmp = Tmp::new("budget-key");
    let path = tmp.path().join("aterm.toml");
    let read = |text: &str| {
        aterm_uds::atomic_file::write(&path, text.as_bytes(), 0o600).expect("write aterm.toml");
        cli::disk_config_read(Some(&path))
    };

    let written = read("[disk]\ncap_used_gib = 466\n");
    assert_eq!(written.config.cap_used_gib, Cap::Written(466));
    assert!(written.notes.is_empty(), "{:?}", written.notes);
    assert!(!written.off_by_fault);
    assert_eq!(
        written.config.removal_view(),
        Config::default(),
        "the key moves no other knob"
    );
    assert_eq!(
        read("[disk]\napply = true\n").config.cap_used_gib,
        Cap::Unwritten
    );
    let zero = read("[disk]\ncap_used_gib = 0\n");
    assert_eq!(zero.config.cap_used_gib, Cap::Off);
    assert!(
        zero.notes.is_empty() && !zero.off_by_fault,
        "{:?}",
        zero.notes
    );
    assert!(cli::disk_faults("[disk]\ncap_used_gib = 0\n").is_empty());

    let text = "[disk]\napply = true\ncap_used_gib = \"500GB\"\n";
    let wrong = read(text);
    assert_eq!(wrong.config.cap_used_gib, Cap::Off);
    assert_eq!(wrong.config.budget(926 * GIB, 779 * GIB, 381 * GIB), None);
    assert!(!wrong.off_by_fault, "{:?}", wrong.notes);
    assert!(wrong.config.apply, "the removal grant stands");
    assert_eq!(wrong.config.auto_free_gib, DEFAULT_AUTO_FREE_GIB);
    assert_eq!(wrong.floor_gib, Some(DEFAULT_AUTO_FREE_GIB));
    assert_eq!(wrong.notes.len(), 1, "{:?}", wrong.notes);
    assert!(
        wrong.notes[0].contains("disk.cap_used_gib must be a whole number, not a string")
            && wrong.notes[0].contains("the rebuildable budget is off"),
        "{:?}",
        wrong.notes
    );
    let faults = cli::disk_faults(text);
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert_eq!(faults[0].path, "disk.cap_used_gib");
    assert!(!faults[0].removal_off);
    assert_eq!(faults[0].note, wrong.notes[0]);

    let negative = read("[disk]\ncap_used_gib = -1\n");
    assert_eq!(negative.config.cap_used_gib, Cap::Unwritten);
    assert!(!negative.off_by_fault);
    assert!(
        matches!(negative.notes.as_slice(), [n] if n.contains("disk.cap_used_gib is negative")),
        "{:?}",
        negative.notes
    );
    assert!(cli::disk_faults("[disk]\ncap_used_gib = -1\n").is_empty());

    let unread = read("[disk]\ncap_used_gib = \n");
    assert!(unread.off_by_fault);
    assert_eq!(unread.config.cap_used_gib, Cap::Off);
    let not_a_table = read("disk = 5\n");
    assert!(not_a_table.off_by_fault);
    assert_eq!(not_a_table.config.cap_used_gib, Cap::Off);

    // CONTROL: a removal key of the wrong type turns every removal off, and leaves the cap.
    let removal = read("[disk]\napply = true\ncap_used_gib = 466\nauto_free_gib = \"500GB\"\n");
    assert!(removal.off_by_fault);
    assert!(!removal.config.apply);
    assert_eq!(removal.config.cap_used_gib, Cap::Written(466));
}

/// A SIZING BEGINS ONLY ON AN OK LOOK OF THE ACCOUNT'S OWN WATCH WITH NOTHING PRESSING, AND AT
/// MOST ONCE AN HOUR. Each condition alone refuses it (the controls: every other input as on
/// the admitted look), and the hour runs from the last sizing's start, a clock set back
/// making one due.
#[test]
fn a_sizing_begins_on_a_quiet_ok_look_alone_and_at_most_once_an_hour() {
    let cap = Cap::Unwritten;
    assert!(admitted(Level::Ok, true, false, false, cap));
    assert!(admitted(Level::Ok, true, false, false, Cap::Written(466)));
    for level in [Level::Unknown, Level::Low, Level::Short, Level::Emergency] {
        assert!(!admitted(level, true, false, false, cap), "{level:?}");
    }
    assert!(
        !admitted(Level::Ok, false, false, false, cap),
        "another process's look"
    );
    assert!(
        !admitted(Level::Ok, true, true, false, cap),
        "a demand stands"
    );
    assert!(
        !admitted(Level::Ok, true, false, true, cap),
        "a projection stands"
    );
    assert!(
        !admitted(Level::Ok, true, false, false, Cap::Off),
        "the budget is off"
    );

    let at = 1_000_000;
    assert!(due(None, at));
    assert!(!due(Some(at), at));
    assert!(!due(Some(at), at + EVERY_S - 1));
    assert!(due(Some(at), at + EVERY_S));
    assert!(due(Some(at), at - 1), "a clock set back");
}

/// A SIZING COUNTS EACH REBUILDABLE BYTE ONCE, ON ITS VOLUME, BEFORE ITS DEADLINE: a cache and
/// two build directories give the sum of their sizes; a directory inside a cache already sized
/// whole adds nothing; a path named twice counts once; a directory on another volume is left
/// out; and a deadline already passed sizes nothing and says the figure is a floor.
#[test]
fn a_sizing_counts_each_rebuildable_byte_once_on_its_volume() {
    let tmp = Tmp::new("budget-size");
    let t = tmp.path();
    let cache = lay_target(&t.join("cache.noindex"));
    let inside = lay_target(&cache.join("ws/target"));
    let a = lay_target(&t.join("a/target"));
    let b = lay_target(&t.join("b/target"));
    let elsewhere = lay_target(&t.join("mounted/target"));
    let whole = |p: &Path| {
        let (bytes, partial) = target::total_bytes(p);
        assert!(!partial, "{} sized whole", p.display());
        assert!(bytes > 0, "{} holds bytes", p.display());
        bytes
    };
    let here = |p: &Path| !p.starts_with(t.join("mounted"));
    let later = Instant::now() + Duration::from_secs(60);

    let sized = size(
        std::slice::from_ref(&cache),
        &[
            inside.clone(),
            a.clone(),
            b.clone(),
            a.clone(),
            elsewhere.clone(),
        ],
        &here,
        later,
        &|_| true,
    );
    assert_eq!(
        sized,
        Rebuildable {
            bytes: whole(&cache) + whole(&a) + whole(&b),
            partial: false,
        }
    );
    // CONTROL: on its own volume the other directory counts.
    let all = size(
        std::slice::from_ref(&cache),
        &[a.clone(), b.clone(), elsewhere.clone()],
        &|_| true,
        later,
        &|_| true,
    );
    assert_eq!(all.bytes, sized.bytes + whole(&elsewhere));

    let spent = size(&[cache], &[a, b], &here, Instant::now(), &|_| true);
    assert_eq!(
        spent,
        Rebuildable {
            bytes: 0,
            partial: true,
        }
    );
}

/// THE SIZING OVER A HOME: what an unpaged search finds on the volume, each tree once; a
/// directory the volume test refuses (another device) is left out; a search cut short makes
/// the figure a floor; and `cli::disk_rebuildable` over the same home, which compares real
/// volume ids, counts both directories (one volume here).
#[test]
fn the_sizing_over_a_home_counts_what_its_search_finds_on_the_volume() {
    let tmp = Tmp::new("budget-home");
    let home = tmp.path().join("home");
    let a = lay_target(&home.join("a/target"));
    let b = lay_target(&home.join("b/target"));
    let bytes = |p: &Path| target::total_bytes(p).0;
    let roots = discover::Roots {
        home: Some(home.clone()),
        claude_tmp: Some(tmp.path().join("claude-tmp")),
        ..discover::Roots::default()
    };
    let later = Instant::now() + Duration::from_secs(60);
    let not_b = |p: &Path| !p.ends_with("b/target");

    let one = rebuildable(&roots, discover::Budget::SHIPPED, later, &not_b);
    assert_eq!(
        one,
        Rebuildable {
            bytes: bytes(&a),
            partial: false,
        }
    );
    let both = rebuildable(&roots, discover::Budget::SHIPPED, later, &|_| true);
    assert_eq!(both.bytes, bytes(&a) + bytes(&b));
    assert!(!both.partial);

    let cut = rebuildable(
        &roots,
        discover::Budget {
            found: 1,
            ..discover::Budget::SHIPPED
        },
        later,
        &|_| true,
    );
    assert!(cut.partial, "a search cut short is a floor");
    assert!(cut.bytes == bytes(&a) || cut.bytes == bytes(&b), "{cut:?}");

    let real = cli::disk_rebuildable(&home, &roots, None, later);
    assert_eq!(real, both);
}

/// THE SHARED DEPENDENCY STORE COUNTS IN THE REBUILDABLE TOTAL (DC-17g): the store targo fills
/// under a home's `.cargo` (`atpkg::build_cache::shared_units::root`), which no search for build
/// directories reaches, adds its bytes to the budget's sizing and is a holder of the survey's
/// total (`seen`). NEGATIVE CONTROL: the same home before the store is laid counts its build
/// directory alone, and its survey holds no holder.
#[test]
fn the_shared_dependency_store_counts_in_the_rebuildable_total() {
    let tmp = Tmp::new("budget-store");
    let home = tmp.path().join("home");
    let a = lay_target(&home.join("a/target"));
    let roots = discover::Roots {
        home: Some(home.clone()),
        claude_tmp: Some(tmp.path().join("claude-tmp")),
        ..discover::Roots::default()
    };
    let later = Instant::now() + Duration::from_secs(60);
    let bytes = |p: &Path| target::total_bytes(p).0;
    let observed = || {
        let mut survey = crate::harness::disk::Survey::new(0);
        survey.home = Some(home.clone());
        survey.volume_device = target::dev_of(&home);
        crate::harness::disk::observe_registered_holders(&mut survey);
        survey
    };

    // CONTROL: no store yet.
    let without = rebuildable(&roots, discover::Budget::SHIPPED, later, &|_| true);
    assert_eq!(without.bytes, bytes(&a));
    assert!(observed().registered_holders.is_empty());

    let store = home.join(".cargo/trust/native-dependencies-v1");
    assert_eq!(atpkg::build_cache::shared_units::root(&home), store);
    let unit = store.join("build/serde/0123abcd");
    std::fs::create_dir_all(unit.join("out")).unwrap();
    std::fs::write(unit.join(".lock"), b"").unwrap();
    std::fs::write(unit.join("out/libserde.rlib"), vec![7u8; 64 * 1024]).unwrap();
    assert!(bytes(&store) >= 64 * 1024);

    let with = rebuildable(&roots, discover::Budget::SHIPPED, later, &|_| true);
    assert_eq!(
        with,
        Rebuildable {
            bytes: bytes(&a) + bytes(&store),
            partial: false,
        }
    );
    let survey = observed();
    assert_eq!(survey.registered_holders.len(), 1);
    assert_eq!(survey.registered_holders[0].path, store);
    assert_eq!(survey.registered_holders[0].bytes, bytes(&store));
    let seen = crate::harness::disk::report(&survey, Config::default()).seen;
    assert_eq!(seen.bytes, bytes(&store));
}

/// A REGISTERED CACHE WHOSE REGISTRATION CHANGES WHILE IT IS SIZED IS NOT COUNTED: sized and
/// then no longer current, its bytes are left out and the figure is a floor, as the report's
/// holders re-check a cache after its walk. A build directory found by the search is counted
/// either way: only a registration can change under the walk. CONTROL: the same cache, still
/// current, counts.
#[test]
fn a_registered_cache_whose_registration_changes_while_sized_is_not_counted() {
    let tmp = Tmp::new("budget-current");
    let t = tmp.path();
    let cache = lay_target(&t.join("cache.noindex"));
    let found = lay_target(&t.join("a/target"));
    let later = Instant::now() + Duration::from_secs(60);
    let bytes = |p: &Path| target::total_bytes(p).0;

    let changed = size(
        std::slice::from_ref(&cache),
        std::slice::from_ref(&found),
        &|_| true,
        later,
        &|_| false,
    );
    assert_eq!(
        changed,
        Rebuildable {
            bytes: bytes(&found),
            partial: true,
        }
    );

    // CONTROL: still current once sized, the cache counts and the figure is whole.
    let current = size(
        std::slice::from_ref(&cache),
        std::slice::from_ref(&found),
        &|_| true,
        later,
        &|_| true,
    );
    assert_eq!(
        current,
        Rebuildable {
            bytes: bytes(&cache) + bytes(&found),
            partial: false,
        }
    );
}

/// A SIZING WHOSE VOLUME CANNOT BE PLACED READS AS A FLOOR: `cli::disk_rebuildable` of a volume
/// path that names nothing counts no byte and says the figure is "at least", never a complete
/// 0. CONTROL: over a home that exists, the same sizing is whole.
#[test]
fn a_sizing_whose_volume_cannot_be_placed_reads_as_a_floor() {
    let tmp = Tmp::new("budget-unplaced");
    let home = tmp.path().join("home");
    lay_target(&home.join("a/target"));
    let roots = discover::Roots {
        home: Some(home.clone()),
        claude_tmp: Some(tmp.path().join("claude-tmp")),
        ..discover::Roots::default()
    };
    let later = Instant::now() + Duration::from_secs(60);

    let unplaced = cli::disk_rebuildable(&tmp.path().join("no-such-volume"), &roots, None, later);
    assert_eq!(
        unplaced,
        Rebuildable {
            bytes: 0,
            partial: true,
        }
    );

    // CONTROL: a volume that is there is placed, and its sizing is whole.
    let placed = cli::disk_rebuildable(&home, &roots, None, later);
    assert!(!placed.partial, "{placed:?}");
    assert!(placed.bytes > 0, "{placed:?}");
}

/// THE RECORD EVERY PROCESS SHARES: a claim in an empty folder begins a sizing and stamps it;
/// a second claim within the hour (another process's) finds it not due and reads the stamp; a
/// kept sizing reads back whole, so a new process prints it with its true age; a claim an
/// hour on begins again and keeps the last sizing; a sizing kept after another claim is
/// refused; and a file that is not a record is claimed over.
#[test]
fn the_budgets_record_is_claimed_once_an_hour_across_processes() {
    let tmp = Tmp::new("budget-record");
    let dir = tmp.path().join("state");
    let at = 1_000_000;
    assert_eq!(Record::load(&dir), Record::default());
    assert_eq!(
        claim(&dir, at).expect("claim"),
        Claim::Claimed(Record::default())
    );
    let stamped = Record {
        began_unix: Some(at),
        last: None,
    };
    assert_eq!(claim(&dir, at + 1).expect("claim"), Claim::NotDue(stamped));

    let sizing = Sizing {
        volume: "container:disk3".to_owned(),
        total: 926 * GIB,
        used: 779 * GIB,
        sized: Rebuildable {
            bytes: 381 * GIB,
            partial: true,
        },
        sized_unix: at + 90,
    };
    keep(&dir, at, sizing.clone()).expect("keep");
    let kept = Record {
        began_unix: Some(at),
        last: Some(sizing.clone()),
    };
    assert_eq!(Record::load(&dir), kept);
    assert_eq!(Record::decode(&kept.encode()), Some(kept.clone()));

    assert_eq!(
        claim(&dir, at + EVERY_S).expect("claim"),
        Claim::Claimed(kept)
    );
    let again = Record::load(&dir);
    assert_eq!(again.began_unix, Some(at + EVERY_S));
    assert_eq!(again.last, Some(sizing.clone()), "the last sizing stands");
    assert!(keep(&dir, at, sizing).is_err(), "claimed again since");

    aterm_uds::atomic_file::write(&dir.join(FILE), b"not a record", 0o600).expect("garble");
    assert_eq!(Record::load(&dir), Record::default());
    assert!(matches!(
        claim(&dir, at + EVERY_S + 1).expect("claim"),
        Claim::Claimed(_)
    ));
    assert!(claim(&dir, 0).is_err(), "an unread clock claims nothing");
}

/// The sizing of a 1000 GiB volume `v` with `used` GiB used, `rebuildable` GiB of them counted.
fn sized(used: u64, rebuildable: u64) -> Sizing {
    Sizing {
        volume: "v".to_owned(),
        total: 1000 * GIB,
        used: used * GIB,
        sized: Rebuildable {
            bytes: rebuildable * GIB,
            partial: false,
        },
        sized_unix: 1,
    }
}

/// [`lay_target`] at `<home>/<name>/target`, answered canonical (the scan names the real path).
#[cfg(unix)]
fn laid(home: &Path, name: &str) -> PathBuf {
    std::fs::canonicalize(lay_target(&home.join(name).join("target"))).expect("canonical")
}

/// A budget look over the named `targets` at `now`, 100 GiB free.
#[cfg(unix)]
fn look<'a>(
    state: &'a Path,
    volume: &'a Path,
    targets: &'a [PathBuf],
    now: i64,
) -> cli::DiskLook<'a> {
    cli::DiskLook {
        state,
        volume,
        free: Some(100 * GIB),
        targets,
        discover: None,
        transcripts: None,
        now,
        room: None,
        table: None,
        demand: None,
        projected: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    }
}

/// The real remover the pass runs, at `now`.
#[cfg(unix)]
fn real_remover(now: i64, config: Config) -> impl FnMut(&Row) -> io::Result<Removed> {
    crate::harness::disk::remover(
        Judge {
            now,
            threshold_days: config.target_stale_days,
            device: None,
            volume: None,
        },
        None,
        idle::Sessions::default,
    )
}

/// The rows of `kind` in `state`'s disk ledger.
#[cfg(unix)]
fn ledger(state: &Path, kind: &str) -> Vec<Value> {
    std::fs::read_to_string(state.join(cli::DISK_LEDGER))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| aterm_json::from_str::<Value>(line).ok())
        .filter(|row| row.get("kind").and_then(Value::as_str) == Some(kind))
        .collect()
}

/// Whether `target`'s profile still holds its incremental caches.
#[cfg(unix)]
fn has_incremental(target: &Path) -> bool {
    target.join("debug").join("incremental").exists()
}

/// One page of the budget pass over `targets` at `now`, free space always measured at 100 GiB.
#[cfg(unix)]
fn page(
    state: &Path,
    volume: &Path,
    targets: &[PathBuf],
    now: i64,
    config: Config,
    budget: &cli::BudgetLook<'_>,
) -> cli::BudgetTick {
    cli::disk_tick_budget(
        &look(state, volume, targets, now),
        config,
        budget,
        &mut || Ok(()),
        &mut real_remover(now, config),
        &mut || Some(100 * GIB),
        &mut discover::Cursor::new(),
    )
}

/// OVER A CAP BELOW THE KEPT BYTES, ONE PASS TAKES EVERY ELIGIBLE ROW AND SAYS SO ONCE. A 1000 GiB
/// volume with 900 GiB used, 50 of them rebuildable, under an 800 GiB cap keeps 850 GiB aterm
/// cannot rebuild: no pass can meet the 200 GiB free the cap leaves. The pass reads `disk=ok`,
/// takes the idle caches of `a` and `b`, keeps `live` (a session works there), and writes one
/// `cannot-meet` row; the next pass writes none. CONTROL: the memo gone (as a memo held in
/// memory is at a restart), the next pass writes a second row.
#[cfg(unix)]
#[test]
fn on_a_volume_whose_kept_bytes_exceed_the_cap_one_pass_takes_every_eligible_row_and_writes_one_cannot_meet()
 {
    let tmp = Tmp::new("budget-cannot-meet");
    let (home, state) = (tmp.path().join("home"), tmp.path().join("state"));
    let targets = [laid(&home, "a"), laid(&home, "b"), laid(&home, "live")];
    let now = now_after_write(3);
    let config = Config {
        cap_used_gib: Cap::Written(800),
        ..Config::default()
    };
    let sizing = sized(900, 50);
    let over_cap = over(&sizing, config.cap_used_gib, "v", 100 * GIB).expect("over its cap");
    assert_eq!(
        (
            over_cap.budget.cap,
            over_cap.budget.kept,
            over_cap.free_target
        ),
        (800 * GIB, 850 * GIB, 200 * GIB)
    );
    let level = config
        .thresholds_with(Some(sizing.total), Written::default(), None)
        .level(Some(100 * GIB), None);
    assert_eq!(level, Level::Ok);
    let live = Live {
        cwds: vec![std::fs::canonicalize(home.join("live")).expect("canonical")],
        ..Live::default()
    };
    let budget = cli::BudgetLook {
        sizing: &sizing,
        over: over_cap,
        live: &live,
        page: 1,
    };

    let first = page(&state, tmp.path(), &targets, now, config, &budget);
    assert_eq!(
        first.end,
        cli::BudgetEnd::CannotMeet {
            row: true,
            partial: false
        },
        "{:?}",
        first.tick
    );
    assert!(!has_incremental(&targets[0]) && !has_incremental(&targets[1]));
    assert!(has_incremental(&targets[2]), "a live session's build stays");
    let second = page(&state, tmp.path(), &targets, now, config, &budget);
    assert_eq!(
        second.end,
        cli::BudgetEnd::CannotMeet {
            row: false,
            partial: false
        }
    );
    let rows = ledger(&state, CANNOT_MEET_ROW_KIND);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].get("cap_bytes").and_then(Value::as_u64),
        Some(800 * GIB)
    );
    assert_eq!(
        rows[0].get("kept_bytes").and_then(Value::as_u64),
        Some(850 * GIB)
    );
    assert_eq!(rows[0].get("held").and_then(Value::as_u64), Some(1));
    assert!(state.join(UNMET_FILE).is_file());

    // CONTROL: without the memo on disk, the next pass says it again.
    std::fs::remove_file(state.join(UNMET_FILE)).expect("the memo");
    let third = page(&state, tmp.path(), &targets, now, config, &budget);
    assert_eq!(
        third.end,
        cli::BudgetEnd::CannotMeet {
            row: true,
            partial: false
        }
    );
    assert_eq!(ledger(&state, CANNOT_MEET_ROW_KIND).len(), 2);
}

/// Four idle build directories under `home`: one a live session works in, one a live session's
/// build started in, one whose runs used it now (`.aterm-used`), and one nobody keeps.
#[cfg(unix)]
fn four(home: &Path) -> [PathBuf; 4] {
    let targets = [
        laid(home, "cwd"),
        laid(home, "started"),
        laid(home, "used"),
        laid(home, "free"),
    ];
    std::fs::write(targets[2].join(atpkg::build_cache::stale::USED), b"")
        .expect("the record of its runs");
    targets
}

/// A BUDGET PASS TAKES NOTHING A LIVE SESSION'S RECENT BUILDS USED. Of four idle build
/// directories it takes only the one no live session keeps, journals only that one's removal,
/// and its ask names each kept one with why: a session works there, a live session's build
/// started there, its runs used it two days ago (inside the three days their record keeps).
/// CONTROL: with no live session and the record four days old, all four go.
#[cfg(unix)]
#[test]
fn a_budget_pass_takes_nothing_a_live_sessions_recent_build_used() {
    let tmp = Tmp::new("budget-live");
    let (home, state) = (tmp.path().join("home"), tmp.path().join("state"));
    let targets = four(&home);
    let [cwd, started, used, free] = targets.clone();
    let now = now_after_write(2);
    let config = Config {
        cap_used_gib: Cap::Written(800),
        ..Config::default()
    };
    let sizing = sized(900, 50);
    let over_cap = over(&sizing, config.cap_used_gib, "v", 100 * GIB).expect("over its cap");
    let session = "s-0123456789abcdef0123".to_owned();
    let workspace = started.parent().expect("its workspace").to_path_buf();
    let live = Live {
        sessions: BTreeSet::from([session.clone()]),
        starts: vec![atpkg::build_start::Start {
            session,
            workspace,
            build: Some(started.clone()),
            pid: 1,
            at_ms: 0,
        }],
        starts_unknown: false,
        cwds: vec![cwd.parent().expect("its checkout").to_path_buf()],
        cwd_unknown: None,
    };

    let roots = Roots {
        volume: Some(tmp.path().to_path_buf()),
        targets: targets.to_vec(),
        budget: Some(over_cap.free_target),
        ..Roots::default()
    };
    let survey = crate::harness::disk::scan(&roots, now, Trigger::Tick, config, Some(100 * GIB));
    let asked = ask(&survey, &live, over_cap.free_target, None, now);
    assert_eq!(asked.eligible, BTreeSet::from([free.clone()]));
    let held: BTreeMap<PathBuf, String> = asked.held.into_iter().collect();
    assert!(held[&cwd].contains("a session works in"), "{held:?}");
    assert!(held[&started].contains("started a build"), "{held:?}");
    assert!(held[&used].contains("its runs used it"), "{held:?}");

    let budget = cli::BudgetLook {
        sizing: &sizing,
        over: over_cap,
        live: &live,
        page: 1,
    };
    let done = page(&state, tmp.path(), &targets, now, config, &budget);
    assert!(
        matches!(done.end, cli::BudgetEnd::CannotMeet { .. }),
        "{done:?}"
    );
    assert!(!has_incremental(&free), "nobody keeps it");
    for kept in [&cwd, &started, &used] {
        assert!(has_incremental(kept), "{} is kept", kept.display());
    }
    let removing: Vec<String> = ledger(&state, "removing")
        .iter()
        .filter_map(|row| row.get("path").and_then(Value::as_str).map(str::to_owned))
        .collect();
    assert_eq!(removing, [free.to_string_lossy().into_owned()]);

    // CONTROL: nobody live, the record four days old: every one goes.
    let tmp = Tmp::new("budget-live-control");
    let (home, state) = (tmp.path().join("home"), tmp.path().join("state"));
    let targets = four(&home);
    let aged = std::time::SystemTime::now() - Duration::from_secs(4 * 86_400);
    std::fs::File::open(targets[2].join(atpkg::build_cache::stale::USED))
        .and_then(|f| f.set_modified(aged))
        .expect("age the record");
    let nobody = Live::default();
    let budget = cli::BudgetLook {
        live: &nobody,
        ..budget
    };
    let done = page(&state, tmp.path(), &targets, now, config, &budget);
    assert!(
        matches!(done.end, cli::BudgetEnd::CannotMeet { .. }),
        "{done:?}"
    );
    for gone in &targets {
        assert!(!has_incremental(gone), "{} goes", gone.display());
    }
}

/// A BUDGET PASS RUNS WHILE THE VOLUME READS `ok` AND STOPS AT ITS TARGET. Under a 750 GiB cap
/// a 1000 GiB volume with 100 GiB free is 150 GiB short of the 250 GiB the cap leaves; free
/// space measured 100, 150 and then 260 GiB before each row, the pass takes two of its four
/// rows, stops on the measure, ends met and removes the memo of an earlier episode. CONTROL:
/// the same look with no budget is no pass at all, and the same report plans nothing without
/// the budget's ask.
#[cfg(unix)]
#[test]
fn a_budget_pass_runs_while_the_volume_reads_ok_and_stops_at_its_target() {
    let tmp = Tmp::new("budget-target");
    let (home, state) = (tmp.path().join("home"), tmp.path().join("state"));
    let targets = [
        laid(&home, "a"),
        laid(&home, "b"),
        laid(&home, "c"),
        laid(&home, "d"),
    ];
    let now = now_after_write(3);
    let config = Config {
        cap_used_gib: Cap::Written(750),
        ..Config::default()
    };
    let sizing = sized(900, 300);
    let over_cap = over(&sizing, config.cap_used_gib, "v", 100 * GIB).expect("over its cap");
    assert_eq!(
        (
            over_cap.budget.cap,
            over_cap.budget.kept,
            over_cap.free_target
        ),
        (750 * GIB, 600 * GIB, 250 * GIB)
    );
    let level = config
        .thresholds_with(Some(sizing.total), Written::default(), None)
        .level(Some(100 * GIB), None);
    assert_eq!(level, Level::Ok);
    let live = Live::default();
    let inputs = Inputs::of(&sizing, &over_cap.budget, live.digest(), false);
    assert!(unmet(&state, &inputs, now).expect("an earlier episode's memo"));

    // CONTROL: no budget, no pass, and no plan without the budget's ask.
    let plain = cli::disk_tick(
        &look(&state, tmp.path(), &targets, now),
        config,
        &mut |row| panic!("removed {} with no budget", row.path.display()),
        &mut || panic!("measured with no budget"),
    );
    assert_eq!(plain, cli::DiskTick::Plenty);
    let roots = Roots {
        volume: Some(tmp.path().to_path_buf()),
        targets: targets.to_vec(),
        ..Roots::default()
    };
    let survey = crate::harness::disk::scan(&roots, now, Trigger::Tick, config, Some(100 * GIB));
    let mut report = crate::harness::disk::report(&survey, config);
    assert_eq!(crate::harness::disk::pass_plan(&report), None);
    report.budget = Some(ask(&survey, &live, over_cap.free_target, None, now));
    assert_eq!(
        crate::harness::disk::pass_plan(&report).map(|plan| plan.len()),
        Some(4)
    );

    let script = [100, 150, 260];
    let mut reads = 0;
    let tick = cli::disk_tick_budget(
        &look(&state, tmp.path(), &targets, now),
        config,
        &cli::BudgetLook {
            sizing: &sizing,
            over: over_cap,
            live: &live,
            page: 1,
        },
        &mut || Ok(()),
        &mut real_remover(now, config),
        &mut || {
            let free = script.get(reads).copied().unwrap_or(260);
            reads += 1;
            Some(free * GIB)
        },
        &mut discover::Cursor::new(),
    );
    assert_eq!(tick.end, cli::BudgetEnd::Met, "{:?}", tick.tick);
    let cli::DiskTick::Reclaimed(done, _) = &tick.tick else {
        panic!("the budget admits the look: {:?}", tick.tick);
    };
    assert_eq!(done.removed.len(), 2, "{done:?}");
    let stopped = ledger(&state, "stopped");
    assert_eq!(stopped.len(), 1, "{stopped:?}");
    assert_eq!(
        stopped[0].get("by").and_then(Value::as_str),
        Some("measured")
    );
    assert!(!state.join(UNMET_FILE).exists(), "met ends the episode");
}

/// THE PASS IS THE OWNER WATCH'S, AT ANY LEVEL, AND OFF WITH ITS SWITCHES: `pass_admitted`
/// refuses a look that is not the owner's, one with a demand or a projection (the pressure
/// pass's), a cap that is off and `auto_free_gib = 0`; `over` reads no sizing of another
/// volume, none that counted nothing, and no volume at or above its free target. CONTROL: the
/// owner's quiet look with the budget on is admitted, and a volume under its target is over.
#[test]
fn pass_admission_is_off_with_auto_free_zero_a_demand_a_projection_or_no_cap() {
    let on = Cap::Written(800);
    assert!(pass_admitted(true, false, false, on, DEFAULT_AUTO_FREE_GIB));
    assert!(pass_admitted(true, false, false, Cap::Unwritten, 1));
    assert!(!pass_admitted(true, false, false, on, 0));
    assert!(!pass_admitted(true, true, false, on, DEFAULT_AUTO_FREE_GIB));
    assert!(!pass_admitted(true, false, true, on, DEFAULT_AUTO_FREE_GIB));
    assert!(!pass_admitted(
        true,
        false,
        false,
        Cap::Off,
        DEFAULT_AUTO_FREE_GIB
    ));
    assert!(!pass_admitted(
        false,
        false,
        false,
        on,
        DEFAULT_AUTO_FREE_GIB
    ));

    let sizing = sized(900, 50);
    let over_cap = over(&sizing, on, "v", 100 * GIB).expect("over its cap");
    assert_eq!(over_cap.free_target, 200 * GIB);
    assert_eq!(over(&sizing, on, "w", 100 * GIB), None, "another volume");
    assert_eq!(
        over(&sizing, Cap::Off, "v", 100 * GIB),
        None,
        "the budget off"
    );
    assert_eq!(over(&sizing, on, "v", 200 * GIB), None, "at its target");
    assert_eq!(
        over(&sized(900, 0), on, "v", 100 * GIB),
        None,
        "nothing counted"
    );
}

/// A PASS THAT YIELDS OR COUNTS STANDS DOWN WITHOUT A ROW. Free space read, then not (as a
/// pressure look's yield answers), then read again after the page: the pass takes one row,
/// stops unmeasured, and the page stands down on that stop, not on a missing last measure. A
/// removal that counts 200 GiB released while free space still reads 100 GiB (a snapshot that
/// holds the blocks): the pass takes one row and stops counted. Either way it says nothing of
/// the target and leaves no memo. CONTROL: measured throughout, each removal counted as it
/// was, the same pass takes every row and says it cannot meet the target.
#[cfg(unix)]
#[test]
fn a_pass_that_yields_or_counts_stands_down_without_a_row() {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        /// The second read is yielded to a pressure look.
        Yields,
        /// Each removal counts 200 GiB released, free space unmoved.
        Counts,
        /// Measured throughout, each removal counted as it was.
        Whole,
    }
    let run = |mode: Mode| {
        let tmp = Tmp::new("budget-yield");
        let (home, state) = (tmp.path().join("home"), tmp.path().join("state"));
        let targets = [laid(&home, "a"), laid(&home, "b")];
        let now = now_after_write(3);
        let config = Config {
            cap_used_gib: Cap::Written(800),
            ..Config::default()
        };
        let sizing = sized(900, 50);
        let live = Live::default();
        let over_cap = over(&sizing, config.cap_used_gib, "v", 100 * GIB).expect("over");
        let mut real = real_remover(now, config);
        let mut remove = |row: &Row| -> io::Result<Removed> {
            let mut removed = real(row)?;
            if mode == Mode::Counts {
                removed.bytes = 200 * GIB;
            }
            Ok(removed)
        };
        let mut reads = 0;
        let tick = cli::disk_tick_budget(
            &look(&state, tmp.path(), &targets, now),
            config,
            &cli::BudgetLook {
                sizing: &sizing,
                over: over_cap,
                live: &live,
                page: 1,
            },
            &mut || Ok(()),
            &mut remove,
            &mut || {
                reads += 1;
                (mode != Mode::Yields || reads != 2).then_some(100 * GIB)
            },
            &mut discover::Cursor::new(),
        );
        let rows = ledger(&state, CANNOT_MEET_ROW_KIND).len();
        (tick, rows, state.join(UNMET_FILE).exists(), reads)
    };
    for (mode, by) in [
        (Mode::Yields, StopBy::Unmeasured),
        (Mode::Counts, StopBy::Counted),
    ] {
        let (tick, rows, memo, reads) = run(mode);
        assert!(
            matches!(tick.end, cli::BudgetEnd::StoodDown(_)),
            "{:?}",
            tick.end
        );
        assert_eq!((rows, memo, reads), (0, false, 3));
        let cli::DiskTick::Reclaimed(done, _) = &tick.tick else {
            panic!("{:?}", tick.tick);
        };
        assert_eq!(done.removed.len(), 1, "{done:?}");
        let stops: Vec<StopBy> = done.stops.iter().map(|stop| stop.by).collect();
        assert_eq!(stops, [by], "{done:?}");
    }

    // CONTROL: measured throughout, the target is said unmeetable once.
    let (tick, rows, memo, _) = run(Mode::Whole);
    assert_eq!(
        tick.end,
        cli::BudgetEnd::CannotMeet {
            row: true,
            partial: false
        }
    );
    assert_eq!((rows, memo), (1, true));
    let cli::DiskTick::Reclaimed(done, _) = &tick.tick else {
        panic!("{:?}", tick.tick);
    };
    assert_eq!(done.removed.len(), 2, "{done:?}");
}

/// AN UNFINISHED SEARCH SAYS IT CANNOT MEET THE TARGET ONLY ON ITS LAST PAGE: a page short of
/// the target whose search has pages left is unfinished up to page 7 and unable to meet it on
/// page 8, on what its pages reached (a floor); a run takes one page a look, one run a sizing.
/// CONTROL: a search that reached everything says so on its first page, whole, and a measure at
/// the target is met whatever the page.
#[test]
fn an_unfinished_search_says_cannot_meet_only_on_its_last_page() {
    let short = Some(100 * GIB);
    let target = 200 * GIB;
    let unfinished = Applied {
        unfinished: Some("the automatic cache selection cycle has not exhausted".to_owned()),
        ..Applied::default()
    };
    assert_eq!(
        end_of(&unfinished, short, target, 7, false),
        End::Unfinished
    );
    assert_eq!(
        end_of(&unfinished, short, target, BUDGET_PAGES, false),
        End::CannotMeet { partial: true }
    );
    assert!(matches!(
        end_of(&unfinished, None, target, 1, false),
        End::StoodDown(_)
    ));

    // CONTROL: a whole search, or a measure at the target.
    let whole = Applied::default();
    assert_eq!(
        end_of(&whole, short, target, 1, false),
        End::CannotMeet { partial: false }
    );
    assert_eq!(
        end_of(&unfinished, Some(target), target, 3, false),
        End::Met
    );

    let mut run = Run::default();
    assert_eq!(run.next(10), Some(1));
    for page in 1..BUDGET_PAGES {
        run.ran(10, page, true);
        assert_eq!(run.next(10), Some(page + 1));
    }
    run.ran(10, BUDGET_PAGES, true);
    assert_eq!(run.next(10), None, "eight pages a run");
    assert_eq!(run.next(11), Some(1), "a new sizing begins a new run");
    run.ran(11, 1, false);
    assert_eq!(run.next(11), None, "one run a sizing");
}

/// A PAGE WITH A ROW REFUSED IS UNFINISHED UNTIL ITS LAST PAGE: a page whose search reached
/// every row but one of whose removals was refused (it failed, a build held its lock, the
/// journal could not record it) is unfinished up to page 7, as a later page may take that row,
/// and unable to meet the target on page 8, on what its pages reached (a floor). CONTROL: the
/// same page with nothing refused says it cannot meet the target on page 1, whole.
#[test]
fn a_page_with_a_row_refused_is_unfinished_until_its_last_page() {
    let short = Some(100 * GIB);
    let target = 200 * GIB;
    let held = Refusal::RemoveFailed(PathBuf::from("/h/a/target"), "held".to_owned());
    let refused = Applied {
        denials: vec![held],
        ..Applied::default()
    };
    for page in 1..BUDGET_PAGES {
        assert_eq!(
            end_of(&refused, short, target, page, false),
            End::Unfinished,
            "page {page}"
        );
    }
    assert_eq!(
        end_of(&refused, short, target, BUDGET_PAGES, false),
        End::CannotMeet { partial: true }
    );

    // CONTROL: nothing refused, the whole search says so on its first page.
    assert_eq!(
        end_of(&Applied::default(), short, target, 1, false),
        End::CannotMeet { partial: false }
    );
}

/// AN EPISODE PLANS AGAIN A DAY AFTER ITS LAST PASS: a memo that says the target cannot be met
/// plans no pass on the same volume, cap, rebuildable bytes and live sessions within
/// [`REPLAN_AFTER_S`] of the episode's last pass, and plans one at it, since what kept a row
/// from that pass (its runs' record, an idle window, a build's lock) lapses in days or less.
/// The day runs from the last pass, not from the row: a pass a day on writes no second row and
/// moves it. CONTROL: within the day, another volume or cap, other live sessions or more
/// rebuildable bytes plan one; fewer rebuildable bytes, or nothing changed, do not.
#[test]
fn an_episode_plans_again_a_day_after_its_last_pass() {
    let tmp = Tmp::new("budget-replan");
    let dir = tmp.path().join("state");
    let sizing = sized(900, 50);
    let budget = Cap::Written(800)
        .budget(sizing.total, sizing.used, sizing.sized.bytes)
        .expect("a written cap gives a budget");
    let inputs = Inputs::of(&sizing, &budget, 1, false);
    let at = 1_000_000;
    assert!(plans(load(&dir).as_ref(), &inputs, at), "no memo");
    assert!(unmet(&dir, &inputs, at).expect("the memo is kept"));
    let memo = load(&dir).expect("the memo");
    assert_eq!((memo.written_unix, memo.seen_unix), (at, at));
    assert!(!plans(Some(&memo), &inputs, at + REPLAN_AFTER_S - 1));
    assert!(plans(Some(&memo), &inputs, at + REPLAN_AFTER_S));

    // A day on, the pass writes no second row and moves the day.
    let day = at + REPLAN_AFTER_S;
    assert!(!unmet(&dir, &inputs, day).expect("the memo is kept"));
    let memo = load(&dir).expect("the memo");
    assert_eq!((memo.written_unix, memo.seen_unix), (at, day));
    assert_eq!(Unmet::decode(&memo.encode()).as_ref(), Some(&memo));
    assert!(!plans(Some(&memo), &inputs, day + REPLAN_AFTER_S - 1));
    assert!(plans(Some(&memo), &inputs, day + REPLAN_AFTER_S));

    // CONTROL: within the day, what changed plans a pass, and nothing else does.
    let changed = [
        Inputs {
            volume: "w".to_owned(),
            ..inputs.clone()
        },
        Inputs {
            cap: inputs.cap + GIB,
            ..inputs.clone()
        },
        Inputs {
            live: 2,
            ..inputs.clone()
        },
        Inputs {
            rebuildable: inputs.rebuildable + 1,
            ..inputs.clone()
        },
    ];
    for changed in &changed {
        assert!(plans(Some(&memo), changed, day + 1), "{changed:?}");
    }
    let fewer = Inputs {
        rebuildable: inputs.rebuildable - 1,
        ..inputs.clone()
    };
    assert!(!plans(Some(&memo), &fewer, day + 1));
    assert!(!plans(Some(&memo), &inputs, day + 1));
}

/// A build directory at `path` found as `origin`, sized at 1 GiB, no profile read.
fn found(path: &Path, origin: discover::Origin) -> TargetDir {
    TargetDir {
        path: path.to_path_buf(),
        marker: Some("CACHEDIR.TAG".to_owned()),
        holds_source: None,
        walk_cut_short: None,
        profiles: Vec::new(),
        stages: Vec::new(),
        device: Some(1),
        volume: None,
        probed_recent: false,
        origin,
        owned: discover::Owned::NotOwned,
        total: Some((GIB, false)),
        foreign: None,
        idle: None,
        deps_unwalked: None,
    }
}

/// A BUILD DIRECTORY REACHED THROUGH A LINK IS HELD: the folder it lies in names no checkout (a
/// scratch build's link or a cache mirror's leads there), so nothing says whose builds use it,
/// and a budget pass keeps it with nobody live. CONTROL: the same directory named is eligible.
#[test]
fn a_build_directory_reached_through_a_link_is_held() {
    let tmp = Tmp::new("budget-linked");
    let path = tmp.path().join("elsewhere").join("target");
    let now = 1_000_000;
    let asks = |origin: discover::Origin| {
        let mut survey = Survey::new(now);
        survey.targets = vec![found(&path, origin)];
        ask(&survey, &Live::default(), 200 * GIB, None, now)
    };
    let from = tmp.path().join("checkout").join("target");
    let linked = asks(discover::Origin::Linked { from });
    assert!(linked.eligible.is_empty(), "{linked:?}");
    let [(held, why)] = linked.held.as_slice() else {
        panic!("one held: {linked:?}");
    };
    assert_eq!(held, &path);
    assert!(why.contains("reached through a link"), "{why}");

    // CONTROL: named, it may go.
    let named = asks(discover::Origin::Named);
    assert_eq!(named.eligible, BTreeSet::from([path.clone()]));
    assert!(named.held.is_empty(), "{named:?}");
}

/// A BLIND LOOK HOLDS EVERY BUILD DIRECTORY AND SHARED-CACHE LEAF: where a live session's
/// working directory is unknown, or the build-start record reads no line, `ask` holds each
/// directory with that why and `keeps_leaf` keeps each leaf. CONTROL: with both known, the
/// same directory is eligible and the same leaf is not kept.
#[test]
fn a_blind_look_holds_every_build_directory_and_leaf() {
    let tmp = Tmp::new("budget-blind");
    let workspace = tmp.path().join("ws");
    let path = workspace.join("target");
    let now = 1_000_000;
    let mut survey = Survey::new(now);
    survey.targets = vec![found(&path, discover::Origin::Named)];
    let cwd_unknown = Live {
        cwd_unknown: Some("a session's working directory is not known".to_owned()),
        ..Live::default()
    };
    let unread = Live {
        starts_unknown: true,
        ..Live::default()
    };
    for (live, said) in [
        (&cwd_unknown, "working directory is not known"),
        (&unread, "build-start record could not be read"),
    ] {
        assert!(live.blind(), "{live:?}");
        let asked = ask(&survey, live, 200 * GIB, None, now);
        assert!(asked.eligible.is_empty(), "{asked:?}");
        let [(held, why)] = asked.held.as_slice() else {
            panic!("one held: {asked:?}");
        };
        assert_eq!(held, &path);
        assert!(why.contains(said), "{why}");
        assert!(keeps_leaf(live, &path, &workspace, None, now));
    }

    // CONTROL: both known, the directory may go and the leaf is not kept.
    let known = Live::default();
    assert!(!known.blind());
    let asked = ask(&survey, &known, 200 * GIB, None, now);
    assert_eq!(asked.eligible, BTreeSet::from([path.clone()]));
    assert!(!keeps_leaf(&known, &path, &workspace, None, now));
}

/// THE BUILD-START RECORD KEEPS LIVE SESSIONS' BUILDS AS THE SCAN NAMES THEM: of a record with a
/// live session's start, written through a link to its workspace, and a gone session's,
/// `Live::from_record` keeps the live one, also spelled as its real folders, so the real build
/// directory a scan names is kept; a record that holds bytes and reads no line leaves the pass
/// blind; a missing record, or none, reads no start and is not blind. CONTROL: the start as
/// written alone does not keep the real build directory.
#[cfg(unix)]
#[test]
fn the_build_start_record_keeps_live_sessions_builds_as_the_scan_names_them() {
    let tmp = Tmp::new("budget-starts");
    let real = laid(&tmp.path().join("home"), "ws");
    let workspace = real.parent().expect("its workspace").to_path_buf();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&workspace, &link).expect("a link to the workspace");
    let written = link.join("target");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).expect("the state folder");
    let record = state.join("build-starts.jsonl");
    let sid = |n: u64| format!("s-{n:020x}");
    let (live_id, gone_id) = (sid(10), sid(11));
    for session in [&live_id, &gone_id] {
        let start = atpkg::build_start::Start {
            session: session.clone(),
            workspace: link.clone(),
            build: Some(written.clone()),
            pid: 1,
            at_ms: 1,
        };
        atpkg::build_start::append(&record, &start).expect("a start line");
    }
    let now = 1_000_000;

    let read = |at: &Path| Live::from_record([live_id.clone()], Vec::new(), None, Some(at));
    let live = read(&record);
    assert!(!live.blind(), "{live:?}");
    assert!(
        live.starts.iter().all(|start| start.session == live_id),
        "{live:?}"
    );
    let builds: Vec<&Path> = live
        .starts
        .iter()
        .filter_map(|start| start.build.as_deref())
        .collect();
    assert_eq!(builds, [written.as_path(), real.as_path()], "{live:?}");
    assert!(keeps_leaf(&live, &real, &workspace, None, now));

    let garbage = state.join("garbage.jsonl");
    std::fs::write(&garbage, b"not a start line\n").expect("a record that reads no line");
    let blind = read(&garbage);
    assert!(blind.starts_unknown && blind.blind(), "{blind:?}");
    let none = read(&state.join("missing.jsonl"));
    assert!(none.starts.is_empty() && !none.blind(), "{none:?}");
    let unset = Live::from_record([live_id], Vec::new(), None, None);
    assert!(unset.starts.is_empty() && !unset.blind(), "{unset:?}");

    // CONTROL: the start as its build spelled it, alone, does not reach the real folder.
    let as_written = Live {
        starts: live
            .starts
            .iter()
            .filter(|start| start.workspace == link)
            .cloned()
            .collect(),
        ..live.clone()
    };
    assert_eq!(as_written.starts.len(), 1, "{as_written:?}");
    assert!(!keeps_leaf(&as_written, &real, &workspace, None, now));
}

/// A BUDGET PASS'S TIERS COUNT AS ONE RUN: free space reads 100 GiB before each row (a snapshot
/// that holds the blocks), 100 GiB short of the 200 GiB target, and the tier 2 row's removal
/// counts 200 GiB released: the pass stops counted before the tier 3 row, which a count kept
/// per tier would take. CONTROL: removals that count 50 GiB each cover no shortfall, and both
/// rows go.
#[cfg(unix)]
#[test]
fn a_budget_pass_counts_its_tiers_as_one_run() {
    let now = 1_792_195_200;
    let row = |path: &str, witness: Witness| Row {
        class: Class::CargoTargets,
        path: PathBuf::from(path),
        bytes: 3 * GIB,
        bytes_partial: false,
        witness,
        removable: true,
        blocked: None,
    };
    let stale = row(
        "/h/x/target",
        Witness::StaleBuildDir {
            marker: "CACHEDIR.TAG".to_owned(),
            profiles: 1,
            idle_days: 9,
            threshold_days: 1,
            leaves: Vec::new(),
        },
    );
    let whole = row(
        "/h/v/target",
        Witness::IdleWhole {
            marker: "CACHEDIR.TAG".to_owned(),
            checkout: PathBuf::from("/h/v"),
            profiles: 2,
            last_used: now - 20 * 86_400,
            age_s: 20 * 86_400,
            window_days: 7,
            device: 1,
        },
    );
    let mut survey = Survey::new(now);
    survey.free_bytes = Some(100 * GIB);
    let pass = |counted: u64| {
        let mut rep = crate::harness::disk::report(&survey, Config::default());
        rep.rows = vec![stale.clone(), whole.clone()];
        rep.budget = Some(Ask {
            admit: Admit::Budget {
                free_target: 200 * GIB,
            },
            eligible: BTreeSet::from([stale.path.clone(), whole.path.clone()]),
            held: Vec::new(),
        });
        crate::harness::disk::apply_auto(
            &rep,
            None,
            &mut |row: &Row| {
                Ok(Removed {
                    bytes: counted,
                    units: vec![row.path.clone()],
                    ..Removed::default()
                })
            },
            &mut || Some(100 * GIB),
            &mut |_: &Step<'_>| true,
        )
        .expect("a budget report plans its pass")
    };
    let one = pass(200 * GIB);
    let removed: Vec<&Path> = one
        .removed
        .iter()
        .map(|(row, _)| row.path.as_path())
        .collect();
    assert_eq!(removed, [stale.path.as_path()], "{one:?}");
    let [stop] = one.stops.as_slice() else {
        panic!("one stop: {one:?}");
    };
    assert_eq!(
        (stop.by, stop.short, stop.released, stop.left, stop.tier),
        (StopBy::Counted, 100 * GIB, 200 * GIB, 1, Some(3))
    );

    // CONTROL: what was counted covers no shortfall, and the pass takes both rows.
    let both = pass(50 * GIB);
    assert_eq!(both.removed.len(), 2, "{both:?}");
    assert!(both.stops.is_empty(), "{both:?}");
}

/// The live digest, now one function the emergency's wait reads too, keeps DC-04b's values: the
/// session ids sorted and once, each ended by a 0 byte, then a 1 byte, then the working
/// directories sorted and once, each ended by a 0 byte, through FNV-1a 64, in whatever order
/// they came.
/// NEGATIVE CONTROL: a digest that kept the order given, or counted a working directory twice,
/// fails the reversed line.
#[test]
fn live_digest_keeps_dc_04bs_values() {
    let want = aterm_hash::fnv1a_64(b"a\0b\0\x01/x\0/y\0");
    let live = Live {
        sessions: ["b", "a"].into_iter().map(str::to_owned).collect(),
        cwds: vec![
            PathBuf::from("/y"),
            PathBuf::from("/x"),
            PathBuf::from("/x"),
        ],
        ..Live::default()
    };
    assert_eq!(live.digest(), want);
    assert_eq!(
        live_digest(["a", "b"], [Path::new("/x"), Path::new("/y")]),
        want
    );
    assert_eq!(
        live_digest(
            ["b", "a", "a"],
            [Path::new("/y"), Path::new("/x"), Path::new("/y")],
        ),
        want,
        "reversed and repeated"
    );
    assert_ne!(live_digest(["a"], [Path::new("/x")]), want);
}
