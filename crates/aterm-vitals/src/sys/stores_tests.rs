// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `sys disk`'s `store` lines: each store's figures, the unread word, the warning and
//! the walker's kept history a growth per hour is read from.

use super::*;
use crate::model::Snapshot;
use crate::sys::json::contract_violations;
use crate::sys::view::{render_json, render_text};
use crate::sys::{Ctx, Freshness, Machine, SysQuery, Target};

const MIB: u64 = 1024 * 1024;

/// A bound of `bytes` over the store's files, held as `at` says.
fn bound(bytes: u64, at: AtBound) -> Option<StoreBound> {
    Some(StoreBound {
        bytes,
        at,
        walked: true,
    })
}

/// The vault's records log as this Mac held it on 2026-11-14: 52.8 MB of its 64 MiB, growing.
fn records() -> StoreFact {
    StoreFact {
        word: StoreWord::VaultRecords,
        n: None,
        under: "home",
        rel: Some(".config/aterm/secrets/records".into()),
        size: Some(StoreSize {
            alloc: 52_805_632,
            logical: 52_800_000,
            files: 2,
            age_s: Some(12),
        }),
        bound: bound(64 * MIB, AtBound::Refuses),
        growth_per_h: Some(MIB as i64),
        fill_per_h: Some(MIB as i64),
    }
}

/// A store with no bound that nothing measured yet.
fn packages_unread() -> StoreFact {
    StoreFact {
        word: StoreWord::Packages,
        n: None,
        under: "home",
        rel: Some("Library/Application Support/aterm/pkg".into()),
        size: None,
        bound: None,
        growth_per_h: None,
        fill_per_h: None,
    }
}

/// A store of `word` that discards its oldest at `bound_bytes` and holds `logical`: a ring at
/// its steady state where the two are close.
fn ring(word: StoreWord, logical: u64, bound_bytes: u64) -> StoreFact {
    StoreFact {
        word,
        n: None,
        under: "home",
        rel: Some("Library/Application Support/aterm/ring".into()),
        size: Some(StoreSize {
            alloc: logical,
            logical,
            files: 4,
            age_s: Some(12),
        }),
        bound: bound(bound_bytes, AtBound::Discards),
        growth_per_h: Some(MIB as i64),
        fill_per_h: Some(MIB as i64),
    }
}

/// A Commons table as a walk reads it: its files' bytes, and the bound on the operations it
/// holds, which no walk reads.
fn table(n: u32, logical: u64) -> StoreFact {
    StoreFact {
        word: StoreWord::CommonsTable,
        n: Some(n),
        under: "home",
        rel: Some(".config/aterm/secrets/commons/tables".into()),
        size: Some(StoreSize {
            alloc: logical.next_multiple_of(4096),
            logical,
            files: 5,
            age_s: Some(12),
        }),
        bound: Some(StoreBound {
            bytes: 128 * MIB,
            at: AtBound::Refuses,
            walked: false,
        }),
        growth_per_h: Some(-4096),
        fill_per_h: Some(-4096),
    }
}

/// The disk view's text lines and its JSON `disk` object over `stores`, with a walker's
/// (empty) balance.
fn disk_view(stores: &[StoreFact]) -> (Vec<String>, Jv) {
    let machine = Machine {
        family: "macOS",
        ..Machine::default()
    };
    let target = Target {
        key: 7,
        sid: "s-ab12".into(),
        panes: vec![7],
        ..Target::default()
    };
    let snap = Snapshot::default();
    let totals = crate::sys::DiskTotals::default();
    let ctx = Ctx {
        snap: Some(&snap),
        epoch: 1,
        as_of_unix_ms: 0,
        age_ms: Some(0),
        state: Freshness::Fresh,
        machine: &machine,
        target: &target,
        tabs: &[],
        caller_panes: None,
        event_caller: None,
        owner_class: true,
        volume: None,
        roots: &[],
        disk: Some(&totals),
        stores,
        home: Some("/Users//a"),
        events: &[],
        gap: None,
        head_seq: 0,
        held: None,
    };
    let text = render_text(&SysQuery::parse("disk").unwrap(), &ctx);
    let json = render_json(&SysQuery::parse("disk --json").unwrap(), &ctx);
    let violations = contract_violations(&json);
    assert!(violations.is_empty(), "{violations:?}");
    let disk = json.get("disk").expect("the disk object").clone();
    (text.lines.into_iter().map(|l| l.text).collect(), disk)
}

fn store_lines(lines: &[String]) -> Vec<&str> {
    lines
        .iter()
        .map(String::as_str)
        .filter(|l| l.starts_with("store "))
        .collect()
}

fn summary(lines: &[String]) -> &str {
    lines
        .iter()
        .map(String::as_str)
        .find(|l| l.starts_with("summary "))
        .expect("a summary line")
}

#[test]
fn sys_disk_stores_names_each_store_with_bytes_bound_share_growth_and_full_in() {
    // A store that refuses at its bound and shrinks meets no bound: no `full_in_s`.
    let shrinking = StoreFact {
        word: StoreWord::JournalOutbox,
        rel: Some("Library/Application Support/aterm/alab/outbox.jsonl".into()),
        bound: bound(4 * MIB, AtBound::Refuses),
        size: Some(StoreSize {
            alloc: 2 * MIB,
            logical: 2 * MIB,
            files: 1,
            age_s: Some(12),
        }),
        growth_per_h: Some(-4096),
        fill_per_h: Some(-4096),
        ..records()
    };
    let heard = StoreFact {
        rel: Some("Library/Application Support/aterm/alab/heard".into()),
        ..ring(StoreWord::JournalHeard, 66_000_000, 64 * MIB)
    };
    let (lines, json) = disk_view(&[records(), table(3, 6_752_583), heard, shrinking]);
    // (64 MiB − 52,800,000) × 3600 / 1 MiB an hour = 49,125 s.
    assert_eq!(
        store_lines(&lines),
        [
            "store name=vault-records under=home path=.config/aterm/secrets/records \
             alloc_bytes=52805632 logical_bytes=52800000 files=2 bound_bytes=67108864 \
             at_bound=refuses used_pct=78 growth_bytes_per_h=1048576 full_in_s=49125 walked_s=12",
            // A table's bound is on the operations it holds, which no walk reads: its share and
            // its time to the bound are unread, never a share of its files' bytes.
            "store name=commons-table n=3 under=home path=.config/aterm/secrets/commons/tables \
             alloc_bytes=6754304 logical_bytes=6752583 files=5 bound_bytes=134217728 \
             at_bound=refuses used_pct=unavailable(unread) growth_bytes_per_h=-4096 \
             full_in_s=unavailable(unread) walked_s=12",
            // A store that discards its oldest at its bound has a share and no time to it.
            "store name=journal-heard under=home \
             path=Library/Application%20Support/aterm/alab/heard alloc_bytes=66000000 \
             logical_bytes=66000000 files=4 bound_bytes=67108864 at_bound=discards used_pct=98 \
             growth_bytes_per_h=1048576 walked_s=12",
            "store name=journal-outbox under=home \
             path=Library/Application%20Support/aterm/alab/outbox.jsonl alloc_bytes=2097152 \
             logical_bytes=2097152 files=1 bound_bytes=4194304 at_bound=refuses used_pct=50 \
             growth_bytes_per_h=-4096 walked_s=12",
        ]
    );
    // The store lines stand before the budget, after every line the view already printed.
    let at = |head: &str| lines.iter().position(|l| l.starts_with(head)).unwrap();
    assert!(at("volume ") < at("store ") && at("store ") < at("budget "));
    let stores = match json.get("stores") {
        Some(Jv::Arr(stores)) => stores,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        stores[0].to_text(),
        "{\"name\":\"vault-records\",\"under\":\"home\",\
         \"path\":\".config/aterm/secrets/records\",\"alloc_bytes\":52805632,\
         \"logical_bytes\":52800000,\"files\":2,\"bound_bytes\":67108864,\
         \"at_bound\":\"refuses\",\"used_pct\":78,\"growth_bytes_per_h\":1048576,\
         \"full_in_s\":49125,\"walked_age_s\":12}"
    );
    assert_eq!(stores[1].get("n"), Some(&Jv::Int(3)));
    assert_eq!(stores[1].get("growth_bytes_per_h"), Some(&Jv::Int(-4096)));
    assert_eq!(stores[1].get("used_pct"), Some(&Jv::why(UNREAD)));
    assert_eq!(stores[1].get("full_in_s"), Some(&Jv::why(UNREAD)));
    assert_eq!(stores[2].get("at_bound"), Some(&Jv::s("discards")));
    assert_eq!(stores[2].get("full_in_s"), None);
    assert_eq!(stores[3].get("full_in_s"), None);
    // The time to a bound is the room its logical bytes leave over their own growth, not the
    // allocation's: 14,308,864 bytes left at half a MiB an hour.
    let sparse = StoreFact {
        growth_per_h: Some(4 * MIB as i64),
        fill_per_h: Some(MIB as i64 / 2),
        ..records()
    };
    assert_eq!(sparse.full_in(), Some(FullIn::In(98_251)));
    // Every word is one of the fixed set, each spelled once.
    let words: Vec<&str> = StoreWord::ALL.iter().map(|w| w.word()).collect();
    let mut once = words.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!(once.len(), words.len());
    assert!(
        words
            .iter()
            .all(|w| { w.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') })
    );
}

#[test]
fn sys_disk_stores_reads_an_unmeasured_figure_as_unread_never_zero() {
    let unmeasured = StoreFact {
        size: None,
        growth_per_h: None,
        fill_per_h: None,
        ..records()
    };
    // Measured once: a size, and no second measurement to read a growth from.
    let once = StoreFact {
        growth_per_h: None,
        fill_per_h: None,
        ..records()
    };
    let (lines, json) = disk_view(&[unmeasured.clone(), once, packages_unread()]);
    assert_eq!(
        store_lines(&lines),
        [
            "store name=vault-records under=home path=.config/aterm/secrets/records \
             alloc_bytes=unavailable(unread) logical_bytes=unavailable(unread) \
             files=unavailable(unread) bound_bytes=67108864 at_bound=refuses \
             used_pct=unavailable(unread) growth_bytes_per_h=unavailable(unread) \
             full_in_s=unavailable(unread) walked_s=unavailable(unread)",
            "store name=vault-records under=home path=.config/aterm/secrets/records \
             alloc_bytes=52805632 logical_bytes=52800000 files=2 bound_bytes=67108864 \
             at_bound=refuses used_pct=78 growth_bytes_per_h=unavailable(unread) \
             full_in_s=unavailable(unread) walked_s=12",
            "store name=packages under=home path=Library/Application%20Support/aterm/pkg \
             alloc_bytes=unavailable(unread) logical_bytes=unavailable(unread) \
             files=unavailable(unread) growth_bytes_per_h=unavailable(unread) \
             walked_s=unavailable(unread)",
        ]
    );
    assert!(crate::sys::is_unavailable_word(UNREAD));
    for line in store_lines(&lines) {
        for field in line.split(' ') {
            assert!(!field.ends_with("=0"), "{field} in {line}");
        }
    }
    let unread = Jv::why(UNREAD);
    let stores = match json.get("stores") {
        Some(Jv::Arr(stores)) => stores,
        other => panic!("{other:?}"),
    };
    for key in [
        "alloc_bytes",
        "logical_bytes",
        "files",
        "used_pct",
        "growth_bytes_per_h",
        "full_in_s",
        "walked_age_s",
    ] {
        assert_eq!(stores[0].get(key), Some(&unread), "{key}");
    }
    assert_eq!(stores[1].get("growth_bytes_per_h"), Some(&unread));
    assert_eq!(stores[1].get("full_in_s"), Some(&unread));
    // An unmeasured store warns of nothing and is nobody's worst.
    assert!(!unmeasured.warns());
    assert_eq!(worst(&[unmeasured, packages_unread()]), None);
    // A table's share is unread whatever its files hold: it is nobody's worst and warns of
    // nothing, at 6.7 MB as at 200 MiB.
    for held in [6_752_583, 200 * MIB] {
        let table = table(1, held);
        assert_eq!(table.used_pct(), None);
        assert_eq!(table.full_in(), Some(FullIn::Unread));
        assert!(!table.warns());
        assert_eq!(worst(std::slice::from_ref(&table)), None);
    }
    // A measured empty store is 0, which is a measurement.
    let empty = StoreFact {
        size: Some(StoreSize::default()),
        growth_per_h: Some(0),
        fill_per_h: Some(0),
        ..records()
    };
    assert_eq!(empty.used_pct(), Some(0));
    assert_eq!(empty.full_in(), Some(FullIn::Never));
}

#[test]
fn sys_disk_stores_warns_at_three_quarters_of_a_bound_or_under_48_hours() {
    let sized = |logical: u64, growth: Option<i64>| StoreFact {
        size: Some(StoreSize {
            alloc: logical,
            logical,
            files: 1,
            age_s: Some(1),
        }),
        bound: bound(64 * MIB, AtBound::Refuses),
        growth_per_h: growth,
        fill_per_h: growth,
        ..records()
    };
    // The share: 75 % of 64 MiB is 48 MiB exactly.
    assert!(sized(48 * MIB, Some(0)).warns());
    assert!(!sized(48 * MIB - 1, Some(0)).warns());
    assert!(sized(64 * MIB + 5, None).warns(), "past its bound");
    // The time: 32 MiB left at 1 MiB an hour is 32 hours, under 48; at half that it is 64.
    let soon = sized(32 * MIB, Some(MIB as i64));
    assert_eq!(soon.full_in(), Some(FullIn::In(32 * 3600)));
    assert!(soon.warns());
    let later = sized(32 * MIB, Some(MIB as i64 / 2));
    assert_eq!(later.full_in(), Some(FullIn::In(64 * 3600)));
    assert!(!later.warns());
    // 48 hours exactly does not warn; a second under it does.
    let at_48h = sized(16 * MIB, Some(MIB as i64));
    assert_eq!(at_48h.full_in(), Some(FullIn::In(WARN_FULL_IN_S)));
    assert!(!at_48h.warns());
    assert!(sized(16 * MIB + 1024, Some(MIB as i64)).warns());
    // No bound, no warning, however large or fast.
    let unbound = StoreFact {
        bound: None,
        ..sized(900 * MIB, Some(i64::MAX))
    };
    assert!(!unbound.warns());
    assert_eq!(unbound.full_in(), None);

    // None warns: the summary says so as it always did.
    let (lines, json) = disk_view(&[later.clone(), unbound.clone()]);
    assert_eq!(summary(&lines), "summary warnings none · not measured none");
    assert_eq!(json.get("store_warnings"), Some(&Jv::Arr(Vec::new())));

    // One warning word, naming the fullest store and how many more warn.
    let table = StoreFact {
        word: StoreWord::CommonsTable,
        n: Some(2),
        ..soon.clone()
    };
    let (lines, json) = disk_view(&[later, table, records(), unbound]);
    assert_eq!(
        summary(&lines),
        "summary warnings store-near-bound (vault-records 78% full in 13h; 1 more) · not \
         measured none"
    );
    assert_eq!(
        json.get("store_warnings").map(Jv::to_text).as_deref(),
        Some(
            "[{\"name\":\"vault-records\",\"used_pct\":78,\"full_in_s\":49125},\
             {\"name\":\"commons-table\",\"n\":2,\"used_pct\":50,\"full_in_s\":115200}]"
        )
    );
    assert_eq!(
        worst(&[soon, records()]).map(StoreFact::label).as_deref(),
        Some("vault-records")
    );
}

/// A store that discards its oldest at its bound stands there once it has filled: it sets no
/// warning, is nobody's worst and has no time to its bound, so the store that refuses writes is
/// the one named. NEGATIVE CONTROL: with every bound judged alike, the heard archive at 98 %
/// and the telemetry log at 83 % warn on every Mac that has run a while, the summary reads
/// `store-near-bound (journal-heard 98%; 2 more)` and the worst store is the archive.
#[test]
fn sys_disk_stores_that_discard_their_oldest_at_the_bound_never_warn_or_hide_one_that_refuses() {
    // The telemetry log after three rotations: three full 4 MiB files and the current one.
    let telemetry = ring(StoreWord::Telemetry, 13_981_013, 16 * MIB);
    // The heard archive once it has filled: its oldest days go only past 64 MiB.
    let heard = ring(StoreWord::JournalHeard, 66_000_000, 64 * MIB);
    for store in [&telemetry, &heard] {
        assert!(store.used_pct().is_some_and(|pct| pct >= WARN_PCT));
        assert!(!store.warns(), "{}", store.label());
        assert_eq!(store.full_in(), None, "{}", store.label());
    }
    assert_eq!(heard.used_pct(), Some(98));
    assert_eq!(telemetry.used_pct(), Some(83));
    // Both at their steady state and nothing else near a bound: no warning.
    let (lines, json) = disk_view(&[heard.clone(), telemetry.clone()]);
    assert_eq!(summary(&lines), "summary warnings none · not measured none");
    assert_eq!(json.get("store_warnings"), Some(&Jv::Arr(Vec::new())));
    assert_eq!(worst(&[heard.clone(), telemetry.clone()]), None);

    // Beside the vault's records log at 81 % (this Mac's 54,864,564 bytes of 2026-11-15), the
    // log is the one warning and the worst, though both rings hold a larger share.
    let records = StoreFact {
        size: Some(StoreSize {
            alloc: 54_865_920,
            logical: 54_864_564,
            files: 2,
            age_s: Some(12),
        }),
        growth_per_h: Some(0),
        fill_per_h: Some(0),
        ..records()
    };
    let stores = [heard, telemetry, records];
    let (lines, json) = disk_view(&stores);
    assert_eq!(
        summary(&lines),
        "summary warnings store-near-bound (vault-records 81%) · not measured none"
    );
    assert_eq!(
        json.get("store_warnings").map(Jv::to_text).as_deref(),
        Some("[{\"name\":\"vault-records\",\"used_pct\":81}]")
    );
    assert_eq!(
        worst(&stores).map(StoreFact::label).as_deref(),
        Some("vault-records")
    );
}

fn sample(at_s: u64, alloc: u64) -> StoreSample {
    StoreSample {
        at_s,
        alloc,
        logical: alloc,
        files: 1,
    }
}

#[test]
fn sys_disk_stores_growth_per_hour_reads_two_kept_sizes_ten_minutes_apart_or_more() {
    let mut h = StoreHistory::default();
    assert_eq!(h.growth_per_h("records"), None, "no sample");
    h.note("records", sample(1_000, 10 * MIB));
    assert_eq!(h.growth_per_h("records"), None, "one sample is no rate");
    h.note("records", sample(1_000 + GROWTH_MIN_S - 1, 11 * MIB));
    assert_eq!(h.growth_per_h("records"), None, "under ten minutes apart");
    h.note("records", sample(1_000 + 1800, 11 * MIB));
    assert_eq!(h.growth_per_h("records"), Some(2 * MIB as i64));
    // A store that shrank reads a negative growth.
    h.note("heard", sample(0, 8 * MIB));
    h.note("heard", sample(7200, 6 * MIB));
    assert_eq!(h.growth_per_h("heard"), Some(-(MIB as i64)));
    // The base is the oldest size of the last 24 hours, not one from days before.
    let mut h = StoreHistory::default();
    h.note("records", sample(0, 0));
    h.note("records", sample(3 * 86_400, 30 * MIB));
    h.note("records", sample(3 * 86_400 + 12 * 3600, 42 * MIB));
    assert_eq!(h.growth_per_h("records"), Some(MIB as i64));
    // With none in the last 24 hours it is the newest before them.
    let mut h = StoreHistory::default();
    h.note("records", sample(0, 0));
    h.note("records", sample(86_400, 24 * MIB));
    h.note("records", sample(3 * 86_400, 72 * MIB));
    assert_eq!(h.growth_per_h("records"), Some(MIB as i64));
    // The logical bytes' rate is read between the same two samples.
    let mut h = StoreHistory::default();
    for (at_s, alloc, logical) in [(0, 8 * MIB, MIB), (3600, 12 * MIB, 2 * MIB)] {
        h.note(
            "sparse",
            StoreSample {
                at_s,
                alloc,
                logical,
                files: 1,
            },
        );
    }
    assert_eq!(
        h.growth("sparse"),
        Some(Growth {
            alloc: 4 * MIB as i64,
            logical: MIB as i64
        })
    );
}

/// A store that drained and fills again reads the rate it fills at. NEGATIVE CONTROL: with the
/// base always the oldest sample of the last 24 hours, the outbox below reads −483,958 bytes an
/// hour at its last sample (3 MiB down to 2 MiB over 7,800 s) and so no time to its bound, for
/// as long as the drain stays in the span, while it fills toward its 4 MiB.
#[test]
fn sys_disk_stores_growth_after_a_drain_is_read_from_the_drain_not_from_before_it() {
    let mut h = StoreHistory::default();
    h.note("outbox", sample(0, 3 * MIB));
    // The flusher took it ten minutes later, between two half-hour samples: the low point is
    // kept, though it is not half an hour after the sample before it.
    h.note("outbox", sample(600, 0));
    assert_eq!(
        h.growth_per_h("outbox"),
        Some(-(18 * MIB as i64)),
        "the drain itself is the newest sample: its move over the span"
    );
    h.note("outbox", sample(1200, 180_000));
    assert_eq!(h.rows["outbox"].len(), 3, "the low point stays");
    assert_eq!(
        h.growth_per_h("outbox"),
        Some(1_080_000),
        "ten minutes after the drain, the rate since it"
    );
    for hour in 1..=2u64 {
        h.note("outbox", sample(600 + hour * 3600, hour * MIB));
        assert_eq!(h.growth_per_h("outbox"), Some(MIB as i64), "hour {hour}");
    }
    // The line: 2 MiB left at 1 MiB an hour is two hours, and it warns.
    let filling = StoreFact {
        word: StoreWord::JournalOutbox,
        size: Some(StoreSize {
            alloc: 2 * MIB,
            logical: 2 * MIB,
            files: 1,
            age_s: Some(1),
        }),
        bound: bound(4 * MIB, AtBound::Refuses),
        growth_per_h: h.growth("outbox").map(|g| g.alloc),
        fill_per_h: h.growth("outbox").map(|g| g.logical),
        ..records()
    };
    assert_eq!(filling.full_in(), Some(FullIn::In(7200)));
    assert!(filling.warns());
    // A store that only shrinks reads its move over the whole span, and each sample under
    // half an hour after the last kept one still replaces the one before it.
    let mut h = StoreHistory::default();
    for i in 0..=6u64 {
        h.note("heard", sample(i * 600, (60 - i) * MIB));
    }
    assert_eq!(
        h.rows["heard"].iter().map(|s| s.at_s).collect::<Vec<_>>(),
        [0, 1800, 3600]
    );
    assert_eq!(h.growth_per_h("heard"), Some(-6 * MIB as i64));
    // Two drains: the base is the low point of the last one.
    let mut h = StoreHistory::default();
    for (at_s, alloc) in [
        (0, 3 * MIB),
        (3600, 0),
        (7200, 2 * MIB),
        (10_800, MIB),
        (14_400, 3 * MIB),
    ] {
        h.note("outbox", sample(at_s, alloc));
    }
    assert_eq!(h.growth_per_h("outbox"), Some(2 * MIB as i64));
}

/// Each store of a folder keeps its number while the history holds it. NEGATIVE CONTROL:
/// numbered by position in name order, the table `0a` below takes number 1 and `aa`, `bb` and
/// `cc` each move up one, so a line or a `since=` delta of `commons-table 2` compares two
/// tables.
#[test]
fn sys_disk_stores_of_one_folder_keep_their_numbers_as_others_come_and_go() {
    let mut h = StoreHistory::default();
    let number = |h: &mut StoreHistory, key: &str, at_s: u64| {
        h.note(key, sample(at_s, 1));
        h.number(key)
    };
    assert_eq!(number(&mut h, "tables/aa", 100), 1);
    assert_eq!(number(&mut h, "tables/bb", 100), 2);
    assert_eq!(number(&mut h, "tables/cc", 100), 3);
    // Another folder's stores are numbered apart.
    assert_eq!(number(&mut h, "accounts/claude", 100), 1);
    // A table that sorts before the rest takes the next number and moves none.
    assert_eq!(number(&mut h, "tables/0a", 200), 4);
    assert_eq!(number(&mut h, "tables/bb", 200), 2);
    assert_eq!(h.number_of("tables/aa"), Some(1));
    assert_eq!(h.number_of("tables/zz"), None);
    // The numbers survive the text.
    let kept = StoreHistory::decode(&h.encode());
    assert_eq!(kept, h);
    assert_eq!(kept.number_of("tables/0a"), Some(4));
    // A table the history dropped (unmeasured for seven days) frees its number, and only then
    // does a new table take it.
    for key in ["tables/aa", "tables/cc", "tables/0a"] {
        h.note(key, sample(SAMPLE_KEEP_S + 300, 2));
    }
    h.prune(SAMPLE_KEEP_S + 300);
    assert_eq!(h.number_of("tables/bb"), None);
    assert_eq!(number(&mut h, "tables/dd", SAMPLE_KEEP_S + 400), 2);
    assert_eq!(h.number_of("tables/cc"), Some(3));
    // A damaged text never gives two tables of a folder one number.
    let damaged = StoreHistory::decode(
        "aterm-disk-stores 1\nk tables/aa\nn 1\ns 1 2 3 4\nk tables/bb\nn 1\ns 1 2 3 4\n\
         k tables/cc\nn 0\ns 1 2 3 4\nk other/aa\nn 1\ns 1 2 3 4\nk tables/gone\nn 7\n",
    );
    assert_eq!(damaged.number_of("tables/aa"), Some(1));
    assert_eq!(damaged.number_of("tables/bb"), None);
    assert_eq!(damaged.number_of("tables/cc"), None);
    assert_eq!(damaged.number_of("other/aa"), Some(1));
    assert_eq!(damaged.number_of("tables/gone"), None, "no size, no store");
}

#[test]
fn sys_disk_stores_history_keeps_spaced_sizes_and_survives_its_text() {
    let mut h = StoreHistory::default();
    // Measured every 30 s for two hours: the first, one each half hour, and the newest.
    for i in 0..=240u64 {
        h.note("a store/with space", sample(i * 30, i));
    }
    let kept: Vec<u64> = h.rows["a store/with space"]
        .iter()
        .map(|s| s.at_s)
        .collect();
    assert_eq!(kept, [0, 1800, 3600, 5400, 7200]);
    assert_eq!(h.latest("a store/with space"), Some(sample(7200, 240)));
    // A clock set back drops what it had measured after the new time.
    h.note("a store/with space", sample(2000, 5));
    let kept: Vec<u64> = h.rows["a store/with space"]
        .iter()
        .map(|s| s.at_s)
        .collect();
    assert_eq!(kept, [0, 1800, 2000]);
    // The text round-trips, a key's space and slash included.
    h.note("tables/aa", sample(10, 1));
    h.note("tables/bb", sample(20, 2));
    h.note("tables/bb/deeper", sample(20, 3));
    let text = h.encode();
    assert!(text.len() < HISTORY_MAX_BYTES);
    assert_eq!(StoreHistory::decode(&text), h);
    assert_eq!(
        h.under("tables")
            .map(|(k, s)| (k, s.alloc))
            .collect::<Vec<_>>(),
        [("tables/aa", 1), ("tables/bb", 2)]
    );
    // Another format's text, or a damaged line, restores nothing of it.
    assert_eq!(
        StoreHistory::decode("something else\nk a\ns 1 2 3 4\n"),
        StoreHistory::default()
    );
    let damaged = StoreHistory::decode("aterm-disk-stores 1\nk a\ns 1 2 3\ns x 2 3 4\ns 5 6 7 8\n");
    assert_eq!(
        damaged.latest("a"),
        Some(StoreSample {
            at_s: 5,
            alloc: 6,
            logical: 7,
            files: 8
        })
    );
    // Samples past seven days go, and with them a store that has no other.
    let mut h = StoreHistory::default();
    h.note("old", sample(0, 1));
    h.note("live", sample(0, 1));
    h.note("live", sample(SAMPLE_KEEP_S + 100, 2));
    h.prune(SAMPLE_KEEP_S + 100);
    assert_eq!(h.latest("old"), None);
    assert_eq!(h.rows["live"].len(), 1);
    // The bounds hold: 64 samples a store, 96 stores, the ones measured longest ago dropped.
    let mut h = StoreHistory::default();
    for i in 0..200u64 {
        h.note("one", sample(i * SAMPLE_EVERY_S, i));
        h.note(&format!("store-{i}"), sample(1_000_000 + i, i));
    }
    assert_eq!(h.rows["one"].len(), SAMPLES_KEPT);
    h.prune(1_000_300);
    assert_eq!(h.rows.len(), HISTORY_STORES);
    assert!(h.latest("store-199").is_some());
    assert!(h.latest("store-0").is_none());
    assert!(h.encode().len() < HISTORY_MAX_BYTES);
}
