// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the rebuildable budget: `disk.cap_used_gib`, which looks may size it, what a
//! sizing counts, the record every process shares, and that no level reads any of it.

use super::*;
use crate::harness::cli;
use crate::harness::disk::tests::{Tmp, lay_target};
use crate::harness::disk::{Config, DEFAULT_AUTO_FREE_GIB, Written};

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
    );
    assert_eq!(all.bytes, sized.bytes + whole(&elsewhere));

    let spent = size(&[cache], &[a, b], &here, Instant::now());
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
