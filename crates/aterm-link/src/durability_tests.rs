// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::state::LogWitness;

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("dur")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch")
}

fn publish(sock: &Path, producer: u64, from_seq: u64, n: u64) {
    let mut c = astream_broker::Client::connect(sock).expect("connect");
    for seq in from_seq..from_seq + n {
        c.publish(producer, seq, "/f/f/x/history", b"v=1")
            .expect("acked");
    }
}

fn len(path: &Path) -> u64 {
    std::fs::metadata(path).expect("the log").len()
}

fn cut(log: &Path, len: u64) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(log)
        .unwrap()
        .set_len(len)
        .unwrap();
}

#[test]
fn a_broker_option_value_is_not_a_durability_flag() {
    let base = ["aterm", "link", "broker", "/r/bus.sock"];
    assert_eq!(Durability::of_argv(&base), Durability::Strict);
    let explicit = [
        "aterm",
        "link",
        "broker",
        "/r/bus.sock",
        "--secret-file",
        "secret",
        "--durability",
        "relaxed",
    ];
    assert_eq!(Durability::of_argv(&explicit), Durability::Relaxed);
    let cases = [
        vec![
            "aterm",
            "link",
            "broker",
            "/r/bus.sock",
            "--secret-file",
            "--durability",
            "relaxed",
        ],
        vec![
            "aterm",
            "link",
            "broker",
            "/r/bus.sock",
            "--secret-file",
            "name with --durability relaxed",
            "bus.log",
        ],
        vec![
            "aterm-link",
            "broker",
            "/r/bus.sock",
            "--secret-file",
            "--durability=strict",
            "bus.log",
        ],
    ];
    let got: Vec<_> = cases.iter().map(|args| Durability::of_argv(args)).collect();
    assert_eq!(got, vec![default_for(true); cases.len()]);
}

#[cfg(unix)]
#[test]
fn only_a_recognised_serving_argv_has_a_known_durability() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    let words = |args: &[&str]| {
        args.iter()
            .map(|word| OsString::from(*word))
            .collect::<Vec<_>>()
    };
    let base = words(&["/renamed/aterm", "link", "broker", "/r/bus.sock"]);
    assert_eq!(Durability::try_of_argv(&base), Some(Durability::Strict));
    let mut named = base.clone();
    named.extend(words(&["--durability", "relaxed"]));
    assert_eq!(Durability::try_of_argv(&named), Some(Durability::Relaxed));
    let mut opaque = base.clone();
    opaque.extend(words(&["--secret-file"]));
    opaque.push(OsString::from_vec(
        b"/r/name\xff --durability relaxed".to_vec(),
    ));
    assert_eq!(
        Durability::try_of_argv(&opaque),
        Some(default_for(true)),
        "a secret file whose name spells a tier names none"
    );
    let invalid = [
        vec!["aterm", "link", "broker", "sock", "--secret-file"],
        vec![
            "aterm",
            "link",
            "broker",
            "sock",
            "--durability",
            "relaxed",
            "--durability",
            "strict",
        ],
        vec!["aterm", "link", "broker", "sock", "--durability", "bogus"],
        vec!["aterm", "link", "broker", "sock", "--unrecognised"],
        vec!["aterm", "link", "broker", "--repair", "log"],
        vec!["aterm", "link", "broker", "sock", "--help"],
        vec!["aterm", "link", "broker", "sock", "--key-file", "key"],
        vec!["aterm", "link", "broker", "sock", "extra", "surplus"],
        vec!["aterm", "link", "serve", "sock"],
    ];
    let got: Vec<_> = invalid
        .iter()
        .map(|args| Durability::try_of_argv(args))
        .collect();
    assert!(got.iter().all(Option::is_none));
    let named: Vec<_> = invalid
        .iter()
        .map(|args| Durability::named_of_argv(args))
        .collect();
    assert!(named.iter().all(Option::is_none));
}

#[test]
fn the_word_the_argv_and_the_default() {
    for tier in [Durability::Strict, Durability::Ordered, Durability::Relaxed] {
        assert_eq!(Durability::parse(tier.name()), Ok(tier));
        let words = argv_words(Some(tier));
        assert_eq!(words, ["--durability", tier.name()]);
        let mut argv = ["aterm", "link", "broker", "/r/bus.sock", "/r/bus.log"]
            .map(str::to_string)
            .to_vec();
        argv.extend(words);
        assert_eq!(Durability::named_of_argv(&argv), Some(Some(tier)));
        assert_eq!(Durability::of_argv(&argv), tier);
    }
    assert!(Durability::parse("Relaxed").is_err());
    assert!(Durability::parse("fast").is_err());
    assert_eq!(Durability::default(), Durability::Strict);
    assert!(argv_words(None).is_empty(), "a bare argv names nothing");
    let bare = ["aterm", "link", "broker", "/r/bus.sock", "/r/bus.log"];
    assert_eq!(Durability::named_of_argv(&bare), Some(None));
    assert_eq!(Durability::of_argv(&bare), resolve(None, false));
    assert_eq!(
        resolve(None, false),
        Durability::Strict,
        "unguarded: strict"
    );
    assert_eq!(
        resolve(None, true),
        Durability::Ordered,
        "the guarded broker's default, on every platform: astream decides what is served"
    );
    let guarded = [
        "aterm",
        "link",
        "broker",
        "/r/bus.sock",
        "/r/bus.log",
        "--secret-file",
        "/r/mint.secret",
    ];
    assert_eq!(Durability::named_of_argv(&guarded), Some(None));
    assert_eq!(Durability::of_argv(&guarded), resolve(None, true));
    for named in [Durability::Strict, Durability::Ordered, Durability::Relaxed] {
        for guarded in [false, true] {
            assert_eq!(resolve(Some(named), guarded), named, "{named:?} {guarded}");
        }
    }
    assert_eq!(
        Durability::of_argv(&["broker", "--durability", "bogus"]),
        Durability::Strict,
        "a word the broker would refuse reads as no flag"
    );
}

/// Every broker aterm opens is bounded to [`RESIDENT_BUDGET`] unless the dev seam names
/// another budget, or none. NEGATIVE CONTROL: a value that is no budget leaves the default,
/// never an unbounded broker.
#[test]
fn the_residency_budget_is_aterms_unless_the_seam_names_one() {
    use std::ffi::OsStr;
    assert_eq!(budget_of(None), Some(RESIDENT_BUDGET));
    assert_eq!(budget_of(Some(OsStr::new("0"))), Some(0));
    assert_eq!(budget_of(Some(OsStr::new("4096"))), Some(4096));
    assert_eq!(budget_of(Some(OsStr::new("none"))), None);
    assert_eq!(budget_of(Some(OsStr::new("16MiB"))), Some(RESIDENT_BUDGET));
    assert_eq!(budget_of(Some(OsStr::new(""))), Some(RESIDENT_BUDGET));
}

#[test]
fn ordered_is_served_ordered_with_aterms_table() {
    use astream_broker::{ServedInfo, ServedReason, Tier};
    assert_eq!(
        Durability::Ordered.tier(),
        astream_broker::Durability::Ordered {
            horizon: Some(ORDERED_HORIZON)
        }
    );
    assert_eq!(Durability::Ordered.options().escalation, escalation());
    assert_eq!(Durability::Relaxed.options().escalation, escalation());
    assert!(Durability::Strict.options().escalation.is_empty());
    for tier in [Durability::Strict, Durability::Ordered, Durability::Relaxed] {
        assert_eq!(
            tier.options().resident_budget,
            resident_budget(),
            "{tier:?}"
        );
    }
    let dir = scratch();
    let log = dir.path().join("bus.log");
    let opened = open(&log, None, Durability::Ordered, Some(test_boot(1))).unwrap();
    let served = opened.broker.served_durability();
    let said = describe_served(Durability::Ordered, Some(served), Some(true));
    assert_eq!(served.requested, Tier::Ordered);
    assert_eq!(served.rules, 5, "aterm's five rules");
    if cfg!(target_vendor = "apple") {
        assert_eq!(
            (served.served, served.reason, served.horizon_ms),
            (Tier::Ordered, ServedReason::None, Some(100))
        );
        assert!(said.starts_with("an ack is in order on the disk"), "{said}");
        assert!(
            said.contains("the last 100 ms of records, plus a batch and a full flush"),
            "astream's bound, not a shorter one: {said}"
        );
        assert!(said.contains("its 5 escalation rules name"), "{said}");
        assert!(
            !said.contains(SPARED),
            "a count identifies no table: {said}"
        );
        assert!(!said.contains("loses nothing"), "{said}");
        let (_, lost) = loss(Durability::Ordered, served.horizon_ms, "machine record");
        let promise = Durability::Ordered.promise();
        assert!(
            promise.contains(&lost) && promise.contains(SPARED),
            "{promise} / {lost}"
        );
    } else {
        assert_eq!(
            (served.served, served.reason),
            (Tier::Strict, ServedReason::NotApple)
        );
        assert!(
            said.starts_with("served strict (the ordering barrier exists only on Apple)"),
            "{said}"
        );
        assert!(said.ends_with(STRICT_PROMISE), "{said}");
    }
    let own = describe_own(Durability::Ordered);
    assert!(
        own.starts_with(&Durability::Ordered.promise())
            && own.contains("astream serves it strict where the device cannot keep the order"),
        "`on`'s note: the served promise, and that astream decides where: {own}"
    );
    let older = ServedInfo {
        requested: Tier::Strict,
        served: Tier::Strict,
        reason: ServedReason::None,
        horizon_ms: None,
        rules: 0,
    };
    let said = describe_served(Durability::Ordered, Some(older), Some(true));
    assert!(
        said.starts_with("served strict (its build serves ordered as strict)"),
        "{said}"
    );
    assert!(said.ends_with(STRICT_PROMISE), "{said}");
    let lowered = ServedInfo {
        requested: Tier::Ordered,
        served: Tier::Strict,
        reason: ServedReason::NotBootVolume,
        horizon_ms: Some(1000),
        rules: 4,
    };
    let said = describe_served(Durability::Ordered, Some(lowered), Some(true));
    assert!(said.contains("its log is not on the boot volume"), "{said}");
    let said = describe_served(Durability::Relaxed, Some(older), Some(true));
    assert!(said.starts_with("served strict ("), "{said}");
    assert!(said.ends_with(STRICT_PROMISE), "{said}");
    let relaxed = open(
        &dir.path().join("relaxed.log"),
        None,
        Durability::Relaxed,
        Some(test_boot(1)),
    )
    .unwrap();
    assert_eq!(relaxed.broker.served_durability().rules, 5);
    let said = describe_served(
        Durability::Relaxed,
        Some(relaxed.broker.served_durability()),
        Some(true),
    );
    assert!(said.contains("its 5 escalation rules name"), "{said}");
    assert!(
        !said.contains(SPARED),
        "a count identifies no table: {said}"
    );
    for weak in [Durability::Ordered, Durability::Relaxed] {
        assert!(weak.promise().contains(SPARED), "{weak:?}");
    }
}

#[test]
fn a_weak_tier_is_told_by_what_its_broker_reports() {
    use astream_broker::{ServedInfo, ServedReason, Tier};
    let ordered = ServedInfo {
        requested: Tier::Ordered,
        served: Tier::Ordered,
        reason: ServedReason::None,
        horizon_ms: Some(1000),
        rules: 4,
    };
    let strict = STRICT_PROMISE;
    let said = describe_served(Durability::Ordered, Some(ordered), Some(true));
    assert!(said.starts_with("an ack is in order on the disk"), "{said}");
    assert!(said.contains("the last 1 s"), "{said}");
    assert!(said.contains("its 4 escalation rules name"), "{said}");
    assert!(
        !said.contains(SPARED),
        "four rules identify no table: {said}"
    );
    assert!(
        !said.contains("machine record"),
        "unknown protected classes: {said}"
    );
    assert!(describe_own(Durability::Relaxed).contains(SPARED));
    assert!(!said.contains("unguarded"), "{said}");
    assert!(
        !said.contains(strict) && !said.contains("loses nothing"),
        "{said}"
    );
    let said = describe_served(Durability::Strict, Some(ordered), Some(true));
    assert!(
        said.starts_with("served ordered (its build opened it ordered) — an ack is in order"),
        "{said}"
    );
    assert!(!said.contains("loses nothing"), "{said}");
    let said = describe_served(Durability::Ordered, Some(ordered), Some(false));
    assert!(
        said.ends_with("no principal is bound, so principal-based rules do not match"),
        "{said}"
    );
    let unbounded = ServedInfo {
        horizon_ms: None,
        rules: 1,
        ..ordered
    };
    let said = describe_served(Durability::Ordered, Some(unbounded), None);
    assert!(
        said.contains("every record since the last full flush"),
        "{said}"
    );
    assert!(said.contains("its 1 escalation rule names"), "{said}");
    assert!(
        !said.contains(SPARED),
        "another table spares its own records"
    );
    let hundred = ServedInfo {
        horizon_ms: Some(100),
        ..ordered
    };
    let said = describe_served(Durability::Ordered, Some(hundred), None);
    assert!(said.contains("the last 100 ms of records"), "{said}");
    for tier in [Tier::Ordered, Tier::Relaxed] {
        let bare = ServedInfo {
            requested: tier,
            served: tier,
            horizon_ms: None,
            rules: 0,
            ..ordered
        };
        let asked = Durability::of_tier(tier).unwrap();
        let said = describe_served(asked, Some(bare), Some(false));
        assert!(said.contains("no record is flushed in full"), "{said}");
        assert!(
            !said.contains(SPARED) && !said.contains("machine record"),
            "{said}"
        );
        assert!(!said.contains("unguarded"), "nothing to tell apart: {said}");
    }
    let said = describe_served(Durability::Relaxed, None, None);
    assert!(said.contains("a new identity"), "{said}");
    assert!(!said.contains("resume where the log continues"), "{said}");
    assert_eq!(describe_served(Durability::Strict, None, None), strict);
    assert_eq!(
        Durability::of_tier(Tier::Ordered),
        Some(Durability::Ordered)
    );
    assert_eq!(Durability::of_tier(Tier::Other(9)), None);
}

#[test]
fn only_another_boot_or_an_uncertain_mark_is_unclean() {
    let (one, two) = (test_boot(1), test_boot(2));
    let b1 = Mark::Boot(one);
    assert_eq!(renewal(&Mark::Absent, Some(one)), None);
    assert_eq!(renewal(&Mark::Absent, None), None);
    assert_eq!(renewal(&b1, Some(one)), None);
    assert_eq!(
        renewal(&b1, Some(two)),
        Some(Renewed::Rebooted { was: one, now: two })
    );
    assert_eq!(renewal(&b1, None), Some(Renewed::NoBoot));
    assert_eq!(
        renewal(&Mark::Unreadable, Some(one)),
        Some(Renewed::Unreadable)
    );
    assert_eq!(boot_text(&one), "00000000-0000-0000-0000-000000000101");
    let said = Renewed::Rebooted { was: one, now: two }.describe();
    assert!(
        said.contains(&boot_text(&one)) && said.contains(&boot_text(&two)),
        "{said}"
    );

    let dir = scratch();
    let log = dir.path().join("bus.log");
    assert_eq!(read_mark(&log), Mark::Absent);
    std::fs::write(mark_path(&log), legacy_mark_text(&one)).unwrap();
    assert_eq!(read_mark(&log), b1, "macOS's upper-case text");
    std::fs::write(mark_path(&log), legacy_mark_of(&boot_text(&one))).unwrap();
    assert_eq!(read_mark(&log), b1, "Linux's lower-case text");
    for damaged in [
        "",
        "boot=\n",
        "boot=a b\n",
        "b1\n",
        "boot=\u{7}\n",
        "boot=b1\n",
        "boot=00000000-0000-0000-0000-00000000010\n",
        "boot=00000000-0000-0000-0000-000000000000\n",
    ] {
        std::fs::write(mark_path(&log), damaged).unwrap();
        assert_eq!(read_mark(&log), Mark::Unreadable, "{damaged:?}");
    }
    std::fs::remove_file(mark_path(&log)).unwrap();
    std::os::unix::fs::symlink("gone", mark_path(&log)).unwrap();
    assert_eq!(read_mark(&log), Mark::Unreadable, "a dangling link");
    let target = dir.path().join("real-mark");
    std::fs::write(&target, legacy_mark_text(&one)).unwrap();
    std::fs::remove_file(mark_path(&log)).unwrap();
    std::os::unix::fs::symlink(&target, mark_path(&log)).unwrap();
    assert_eq!(read_mark(&log), Mark::Unreadable, "a live link");
}

fn kernel_boot_text() -> Option<String> {
    if cfg!(target_vendor = "apple") {
        let mut sysctl = aterm_uds::spawnfd::command("/usr/sbin/sysctl");
        sysctl.args(["-n", "kern.bootsessionuuid"]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let out = aterm_uds::deadline::output_until(&mut sysctl, deadline).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        Some(text.trim().to_string())
    }
}

#[test]
fn the_broker_the_lease_and_an_older_builds_mark_name_one_boot() {
    let boot = this_boot();
    if cfg!(any(target_vendor = "apple", target_os = "linux")) {
        assert!(boot.is_some(), "this host's kernel names its boot");
    }
    let Some(boot) = boot else {
        return;
    };
    let kernel_text = kernel_boot_text().expect("the kernel's own text for its boot");

    let dir = scratch();
    let state = crate::state::StateDir::open(dir.path().join("state")).expect("state dir");
    let lease = state.trusted_boot();
    if cfg!(target_vendor = "apple") {
        assert_eq!(lease, Some(boot), "the lease trusts the broker's boot");
    } else {
        assert!(lease.is_none_or(|b| b == boot), "{lease:?} vs {boot:?}");
    }

    let log = dir.path().join("bus.log");
    let sock = dir.path().join("bus.sock");
    let first = {
        let opened = open(&log, None, Durability::Relaxed, this_boot()).expect("open");
        let mut h = opened.broker.serve(&sock).unwrap();
        publish(&sock, 7, 1, 3);
        h.shutdown();
        opened.broker.log_identity()
    };
    std::fs::write(mark_path(&log), legacy_mark_of(&kernel_text)).unwrap();
    assert_eq!(read_mark(&log), Mark::Boot(boot), "{kernel_text:?}");
    let upgraded = open(&log, None, Durability::Relaxed, this_boot()).expect("reopen");
    assert_eq!(upgraded.renewed, None, "the mark is this boot's");
    assert_eq!(upgraded.broker.log_identity(), first, "nothing moves");
    assert_eq!(read_mark(&log), Mark::Absent, "the mark is removed");
    drop(upgraded);

    let other = test_boot(1);
    assert_ne!(other, boot);
    std::fs::write(mark_path(&log), legacy_mark_text(&other)).unwrap();
    let moved = open(&log, None, Durability::Relaxed, this_boot()).expect("reopen");
    assert_eq!(
        moved.renewed,
        Some(Renewed::Rebooted {
            was: other,
            now: boot
        })
    );
    assert_eq!(
        moved.broker.log_identity().generation,
        first.generation + 1,
        "another boot's mark moves the generation once"
    );
}

#[test]
fn a_power_loss_under_relaxed_moves_the_generation_with_lineage() {
    let dir = scratch();
    let log = dir.path().join("bus.log");
    let sock = dir.path().join("bus.sock");
    let (before, witnessed, at4) = {
        let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).expect("open");
        opened.broker.arm_unclean_stop();
        assert_eq!(opened.renewed, None, "a fresh log has nothing to judge");
        assert_eq!(read_mark(&log), Mark::Absent, "no mark is written");
        let mut h = opened.broker.serve(&sock).expect("serve");
        publish(&sock, 7, 1, 4);
        let at4 = len(&log);
        publish(&sock, 7, 5, 6);
        let witnessed = LogWitness {
            high: opened.broker.head(),
            id: Some(opened.broker.log_identity()),
        };
        assert_eq!(witnessed.high, 10);
        h.shutdown();
        (opened.broker.log_identity(), witnessed, at4)
    };
    cut(&log, at4);

    let control = dir.path().join("control.log");
    std::fs::copy(&log, &control).unwrap();
    std::fs::copy(
        astream_broker::identity_path(&log),
        astream_broker::identity_path(&control),
    )
    .unwrap();
    {
        let b = open(&control, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
        assert_eq!(b.broker.head(), 4, "the cut log is a clean prefix");
        assert_eq!(b.broker.log_identity(), before, "under the SAME identity");
        let csock = dir.path().join("c.sock");
        let mut h = b.broker.serve(&csock).unwrap();
        publish(&csock, 8, 1, 7);
        assert_eq!(b.broker.head(), 11);
        assert!(
            !witnessed.replaced_by(b.broker.head(), Some(b.broker.log_identity())),
            "regrown past the high-water, the cut log passes for the one witnessed"
        );
        h.shutdown();
    }

    let opened = open(&log, None, Durability::Relaxed, Some(test_boot(2))).expect("reopen");
    assert_eq!(opened.renewed, None, "astream's own verdict, no mark");
    assert_eq!(opened.broker.head(), 4);
    let now = opened.broker.log_identity();
    assert_eq!(now.id, before.id, "the same log");
    assert_eq!(now.generation, before.generation + 1, "the next generation");
    assert!(witnessed.replaced_by(4, Some(now)));
    let mut h = opened.broker.serve(&sock).unwrap();
    let mut client = astream_broker::Client::connect(&sock).unwrap();
    assert_eq!(client.log_identity().unwrap(), Some(now));
    let lineage = client
        .log_lineage()
        .cloned()
        .expect("the Hello carries lineage");
    assert_eq!(lineage.steps().last(), Some(&(now.generation, 4)));
    assert_eq!(witnessed.continues(Some(now), Some(&lineage)), Some(4));
    assert_eq!(
        witnessed.continues(Some(now), None),
        None,
        "no lineage, no continuation: forget, as before"
    );
    publish(&sock, 8, 1, 7);
    assert!(
        witnessed.replaced_by(opened.broker.head(), Some(now)),
        "grown or not, it is another generation"
    );
    h.shutdown();
}

#[test]
fn a_restart_or_a_clean_stop_keeps_the_identity() {
    let dir = scratch();
    let log = dir.path().join("bus.log");
    let sock = dir.path().join("bus.sock");
    let first = {
        let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
        opened.broker.arm_unclean_stop();
        let mut h = opened.broker.serve(&sock).unwrap();
        publish(&sock, 7, 1, 3);
        h.shutdown();
        opened.broker.log_identity()
    };
    let again = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
    assert_eq!(again.broker.log_identity(), first);
    assert_eq!(again.broker.head(), 3);
    drop(again);
    let rebooted = open(&log, None, Durability::Relaxed, Some(test_boot(2))).unwrap();
    assert_eq!(
        rebooted.broker.log_identity(),
        first,
        "a clean stop's reboot moves nothing"
    );
    rebooted.broker.arm_unclean_stop();
    drop(rebooted);
    let strict = open(&log, None, Durability::Strict, Some(test_boot(3))).unwrap();
    let moved = strict.broker.log_identity();
    assert_eq!(
        (moved.id, moved.generation),
        (first.id, first.generation + 1)
    );
    strict.broker.arm_unclean_stop();
    drop(strict);
    let after_strict = open(&log, None, Durability::Strict, Some(test_boot(4))).unwrap();
    assert_eq!(
        after_strict.broker.log_identity(),
        moved,
        "strict acks were durable"
    );
}

#[test]
fn the_legacy_mark_moves_the_generation_once_and_is_removed() {
    let dir = scratch();
    let log = dir.path().join("bus.log");
    let sock = dir.path().join("bus.sock");
    let (b1, b2, b3, b4) = (test_boot(1), test_boot(2), test_boot(3), test_boot(4));
    let first = {
        let opened = open(&log, None, Durability::Relaxed, Some(b1)).unwrap();
        opened.broker.arm_unclean_stop();
        let mut h = opened.broker.serve(&sock).unwrap();
        publish(&sock, 7, 1, 3);
        h.shutdown();
        opened.broker.log_identity()
    };
    std::fs::write(mark_path(&log), legacy_mark_text(&b1)).unwrap();
    let generations = |id: astream_broker::LogIdentity| (id.id, id.generation);

    let migrated = open(&log, None, Durability::Relaxed, Some(b2)).unwrap();
    assert!(matches!(migrated.renewed, Some(Renewed::Rebooted { .. })));
    assert_eq!(migrated.mark_kept, None);
    assert_eq!(read_mark(&log), Mark::Absent, "the mark is removed");
    let second = migrated.broker.log_identity();
    assert_eq!(
        generations(second),
        (first.id, first.generation + 1),
        "two verdicts, ONE move"
    );
    assert_eq!(
        migrated.broker.served_durability().served,
        astream_broker::Tier::Relaxed
    );
    drop(migrated);
    let clean = open(&log, None, Durability::Relaxed, Some(b3)).unwrap();
    assert_eq!(
        clean.broker.log_identity(),
        second,
        "clean stop, reboot: nothing"
    );
    assert_eq!(read_mark(&log), Mark::Absent, "and no mark written");
    clean.broker.arm_unclean_stop();
    drop(clean);
    let again = open(&log, None, Durability::Relaxed, Some(b4)).unwrap();
    let third = again.broker.log_identity();
    assert_eq!(generations(third), (first.id, second.generation + 1));
    drop(again);

    for tier in [Durability::Relaxed, Durability::Strict] {
        std::fs::write(mark_path(&log), legacy_mark_text(&b4)).unwrap();
        let same = open(&log, None, tier, Some(b4)).unwrap();
        assert_eq!(same.renewed, None, "{tier:?}");
        assert_eq!(same.broker.log_identity(), third, "{tier:?}");
        assert_eq!(read_mark(&log), Mark::Absent, "{tier:?}");
    }
    std::fs::write(mark_path(&log), "boot=\n").unwrap();
    let unknown = open(&log, None, Durability::Strict, None).unwrap();
    assert_eq!(unknown.renewed, Some(Renewed::Unreadable));
    assert_eq!(
        generations(unknown.broker.log_identity()),
        (first.id, third.generation + 1)
    );
    drop(unknown);
    let next = open(&log, None, Durability::Strict, None).unwrap();
    assert_eq!(
        generations(next.broker.log_identity()),
        (first.id, third.generation + 1),
        "the next open moves nothing more"
    );
}

#[test]
fn a_reader_older_than_the_lineage_window_forgets() {
    let dir = scratch();
    let log = dir.path().join("bus.log");
    let sock = dir.path().join("bus.sock");
    let first = {
        let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
        opened.broker.arm_unclean_stop();
        let mut h = opened.broker.serve(&sock).unwrap();
        publish(&sock, 7, 1, 3);
        h.shutdown();
        opened.broker.log_identity()
    };
    let moves = u64::try_from(astream_broker::Lineage::MAX).unwrap() + 1;
    let mut now = first;
    let mut lineage = None;
    for n in 0..moves {
        let opened = open(
            &log,
            None,
            Durability::Relaxed,
            Some(test_boot(u8::try_from(2 + n).unwrap())),
        )
        .unwrap();
        now = opened.broker.log_identity();
        assert_eq!(now.generation, first.generation + n + 1, "one move a boot");
        opened.broker.arm_unclean_stop();
        let mut h = opened.broker.serve(&sock).unwrap();
        {
            let mut c = astream_broker::Client::connect(&sock).unwrap();
            assert_eq!(c.log_identity().unwrap(), Some(now));
            lineage = c.log_lineage().cloned();
        }
        h.shutdown();
    }
    let lineage = lineage.expect("the Hello carries lineage");
    assert_eq!(lineage.steps().len(), astream_broker::Lineage::MAX);
    let witness = |generation| crate::state::LogWitness {
        high: 3,
        id: Some(astream_broker::LogIdentity {
            generation,
            ..first
        }),
    };
    assert_eq!(
        witness(first.generation + 1).continues(Some(now), Some(&lineage)),
        Some(3),
        "every step from the oldest kept on is in the window: resumed"
    );
    assert_eq!(
        witness(first.generation).continues(Some(now), Some(&lineage)),
        None,
        "the step this generation needs has fallen off: another log, forgotten"
    );
}

#[test]
fn a_legacy_mark_alone_moves_the_generation_with_lineage() {
    let dir = scratch();
    let log = dir.path().join("bus.log");
    let sock = dir.path().join("bus.sock");
    let lineage_of_served = |broker: &astream_broker::Broker| {
        let mut h = broker.serve(&sock).unwrap();
        let lineage = {
            let mut c = astream_broker::Client::connect(&sock).unwrap();
            assert_eq!(c.log_identity().unwrap(), Some(broker.log_identity()));
            c.log_lineage().cloned()
        };
        h.shutdown();
        lineage
    };
    let first = {
        let opened = open(&log, None, Durability::Relaxed, Some(test_boot(1))).unwrap();
        let mut h = opened.broker.serve(&sock).unwrap();
        publish(&sock, 7, 1, 3);
        h.shutdown();
        opened.broker.log_identity()
    };
    let witnessed = crate::state::LogWitness {
        high: 3,
        id: Some(first),
    };
    for (n, mark, tier) in [
        (2, legacy_mark_text(&test_boot(1)), Durability::Relaxed),
        (3, "boot=\n".to_string(), Durability::Strict),
    ] {
        std::fs::write(mark_path(&log), mark).unwrap();
        let migrated = open(&log, None, tier, Some(test_boot(n))).unwrap();
        assert!(migrated.renewed.is_some(), "{tier:?}: the mark's verdict");
        let now = migrated.broker.log_identity();
        assert_eq!(
            (now.id, now.generation),
            (first.id, first.generation + u64::from(n) - 1),
            "{tier:?}: one move"
        );
        assert_eq!(migrated.broker.head(), 3);
        let lineage = lineage_of_served(&migrated.broker)
            .unwrap_or_else(|| panic!("{tier:?}: the Hello carries lineage"));
        assert_eq!(
            lineage.steps().last(),
            Some(&(now.generation, 3)),
            "{tier:?}: the generations agree below the head"
        );
        assert_eq!(
            witnessed.continues(Some(now), Some(&lineage)),
            Some(3),
            "{tier:?}: a reader of the old generation resumes, not forgets"
        );
    }
}

#[test]
fn what_open_refuses_and_what_it_leaves_alone() {
    let dir = scratch();
    let log = dir.path().join("bus.log");
    let sock = dir.path().join("bus.sock");
    let first = {
        let opened = open(&log, None, Durability::Relaxed, None).expect("runs, unknown boot");
        opened.broker.arm_unclean_stop();
        let mut h = opened.broker.serve(&sock).unwrap();
        publish(&sock, 7, 1, 2);
        h.shutdown();
        opened.broker.log_identity()
    };
    let crashed = open(&log, None, Durability::Relaxed, None).unwrap();
    assert_eq!(
        crashed.broker.log_identity().generation,
        first.generation + 1,
        "with no boot to compare, even a restart moves the generation"
    );
    drop(crashed);

    for durability in [Durability::Relaxed, Durability::Ordered, Durability::Strict] {
        let missing = dir.path().join("missing");
        let err = open(
            &missing.join("bus.log"),
            None,
            durability,
            Some(test_boot(1)),
        )
        .err()
        .expect("no directory, no log");
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{durability:?}: {err}");
        assert!(!missing.exists(), "{durability:?}: no directory made");
    }

    let held = open(&log, None, Durability::Strict, Some(test_boot(1))).unwrap();
    std::fs::write(mark_path(&log), legacy_mark_text(&test_boot(9))).unwrap();
    let sidecar = std::fs::read(astream_broker::identity_path(&log)).unwrap();
    let err = open(&log, None, Durability::Relaxed, Some(test_boot(1)))
        .err()
        .expect("a second broker on one log");
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
    assert_eq!(
        read_mark(&log),
        Mark::Boot(test_boot(9)),
        "the mark is the holder's"
    );
    assert_eq!(
        std::fs::read(astream_broker::identity_path(&log)).unwrap(),
        sidecar
    );
    drop(held);
}

enum Write {
    /// A publish on `subject` under `who`'s caps.
    Publish(Who, String),
    /// A pure commit of `group` under `who`'s caps.
    Commit(Who, String),
}

/// Whose caps a [`Write`] goes under.
#[derive(Clone, Copy, Debug)]
enum Who {
    /// The person `h-x`, bound by their ring ([`crate::enable::person_rules`]).
    Person,
    /// The node's bridge, bound by its ring ([`crate::enable::node_rules`]).
    Node,
    /// The fleet root's god cap: unbound, so no principal rule sees it.
    God,
}

const TABLE_FLEET: &str = "f";
const TABLE_NODE: &str = "n-00000000000000aa";
const TABLE_PERSON: &str = "h-x";

fn table_writes() -> Vec<(&'static str, Write, bool)> {
    use crate::subject;
    let (f, node, p) = (TABLE_FLEET, TABLE_NODE, TABLE_PERSON);
    vec![
        (
            "a person's halt",
            Write::Publish(Who::Person, subject::person_halt(f, p)),
            true,
        ),
        (
            "a person's answer",
            Write::Publish(Who::Person, subject::post_lane(f, node, "s-1", p, "answer")),
            true,
        ),
        (
            "mail onto a person's lane",
            Write::Publish(
                Who::Node,
                format!("{}/ask", subject::person_lane(f, p, node)),
            ),
            true,
        ),
        (
            "a person's inbox commit",
            Write::Commit(Who::Person, subject::person_inbox_group(f, p)),
            true,
        ),
        (
            "the god cap's halt",
            Write::Publish(Who::God, subject::person_halt(f, "h-y")),
            true,
        ),
        (
            "an agent's post",
            Write::Publish(Who::Node, subject::post_lane(f, node, "s-2", node, "note")),
            false,
        ),
        (
            "a session's presence row",
            Write::Publish(Who::Node, subject::session_face(f, node, "s-1", "presence")),
            false,
        ),
        (
            "the node's ev digest",
            Write::Publish(Who::Node, subject::node_face(f, node, "ev")),
            false,
        ),
        (
            "a bridge's halt ack",
            Write::Publish(Who::Node, subject::node_face(f, node, "ack")),
            false,
        ),
        (
            "the node's inbox commit",
            Write::Commit(Who::Node, subject::inbox_group(f, node)),
            false,
        ),
    ]
}

fn escalated_by(
    tier: Durability,
    table: astream_broker::Escalation,
) -> Option<Vec<(&'static str, bool)>> {
    let dir = scratch();
    let log = dir.path().join("bus.log");
    let sock = dir.path().join("bus.sock");
    let secret = b"a-mint-secret-of-thirty-two-bytes".to_vec();
    let mut opts = tier.options();
    if tier == Durability::Ordered {
        opts.durability = astream_broker::Durability::Ordered { horizon: None };
    }
    opts.escalation = table;
    opts.guard = Some(secret.clone());
    opts.boot = astream_broker::Boot::Known(test_boot(1));
    let broker =
        astream_broker::Broker::open_opts(&log, opts, || Ok(astream_broker::Prepared::Nothing))
            .expect("open");
    if broker.served_durability().served == astream_broker::Tier::Strict {
        return None;
    }
    let mut h = broker.serve(&sock).expect("serve");
    let client = |grants: Vec<String>| {
        let mut c = astream_broker::Client::connect(&sock).expect("connect");
        for grant in grants {
            let cap = astream_cap::mint(&secret, &grant).expect("mint");
            c.attach(&cap.filter, &cap.tag).expect("attach");
        }
        c
    };
    let mut person = client(crate::enable::person_rules(TABLE_FLEET, TABLE_PERSON));
    let mut node = client(crate::enable::node_rules(TABLE_FLEET, TABLE_NODE));
    let mut god = client(vec![format!("/f/{TABLE_FLEET}/>")]);
    let mut seq = 0;
    let mut out = Vec::new();
    for (what, write, _) in table_writes() {
        seq += 1;
        let off = match write {
            Write::Publish(who, subject) => {
                let (c, producer) = match who {
                    Who::Person => (&mut person, astream_cap::producer_id_of(TABLE_PERSON)),
                    Who::Node => (&mut node, astream_cap::producer_id_of(TABLE_NODE)),
                    Who::God => (&mut god, 9_100),
                };
                c.publish(producer, seq, &subject, b"v=1 t=1 state=on")
                    .unwrap_or_else(|e| panic!("{what} on {subject}: {e}"))
                    .0
            }
            Write::Commit(who, group) => {
                let c = match who {
                    Who::Person => &mut person,
                    Who::Node => &mut node,
                    Who::God => &mut god,
                };
                c.commit(&group, 0)
                    .unwrap_or_else(|e| panic!("{what} on {group}: {e}"))
            }
        };
        out.push((what, broker.durable_head() > off));
    }
    h.shutdown();
    Some(out)
}

#[test]
fn each_record_a_person_would_miss_is_escalated() {
    let want: Vec<(&str, bool)> = table_writes()
        .into_iter()
        .map(|(what, _, escalated)| (what, escalated))
        .collect();
    let mut arms = 0;
    for tier in [Durability::Relaxed, Durability::Ordered] {
        match escalated_by(tier, escalation()) {
            Some(got) => {
                assert_eq!(got, want, "{tier:?}");
                arms += 1;
            }
            None => eprintln!("{tier:?}: served strict here, so the arm proves nothing"),
        }
    }
    assert!(arms >= 1, "the relaxed arm runs everywhere");

    let without_fleet = astream_broker::Escalation::new(
        escalation()
            .rules()
            .iter()
            .filter(|r| **r != astream_broker::Escalate::Subject("/f/*/fleet/>".into()))
            .cloned(),
    );
    assert_eq!(without_fleet.rules().len(), escalation().rules().len() - 1);
    let got = escalated_by(Durability::Relaxed, without_fleet).expect("relaxed is served");
    let of = |what: &str| got.iter().find(|(w, _)| *w == what).map(|(_, e)| *e);
    assert_eq!(of("a person's halt"), Some(true), "their principal rule");
    assert_eq!(
        of("the god cap's halt"),
        Some(false),
        "an unbound halt escapes a table with no fleet subject rule"
    );
}

#[test]
fn a_guarded_relaxed_broker_is_still_guarded() {
    let dir = scratch();
    let log = dir.path().join("bus.log");
    let sock = dir.path().join("bus.sock");
    let opened = open(
        &log,
        Some(b"a-mint-secret-of-thirty-two-bytes".to_vec()),
        Durability::Relaxed,
        Some(test_boot(1)),
    )
    .unwrap();
    let mut h = opened.broker.serve(&sock).unwrap();
    let err = astream_broker::Client::connect(&sock)
        .unwrap()
        .publish(1, 1, "/f/f/x", b"anon")
        .unwrap_err();
    assert!(err.to_string().contains("unauthorized"), "{err}");
    h.shutdown();
}
