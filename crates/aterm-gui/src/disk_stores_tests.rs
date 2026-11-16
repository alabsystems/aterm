// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for aterm's own stores as `sys disk` lists them: every folder aterm writes to lies
//! under a root of the aterm scope, each store's place and bound are its own code's, and a line
//! reads the walker's board, then its kept history, and names no table.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::ai::connector::Cli;
use aterm_diskwalk::{Deny, within};
use aterm_vitals::sys::stores::{AtBound, StoreBound};
use aterm_vitals::sys::{StoreFact, StoreHistory, StoreSample, StoreWord};

use super::*;
use crate::diskwalk_host::{StoreRead, own_folders, own_paths};

/// This platform's home for a person `a`.
fn home() -> &'static Path {
    Path::new(if cfg!(target_os = "macos") {
        "/Users//a"
    } else {
        "/home/a"
    })
}

/// Where the product puts aterm's folders under `home`, by the path functions' pure halves: a
/// test binary's own live ones are its scratch root's.
fn product_dirs(home: &Path) -> OwnDirs {
    let platform = || aterm_types::dirs::StatePlatform {
        home: Some(home.to_path_buf()),
        ..Default::default()
    };
    let state = aterm_types::dirs::resolve_state_dir(None, platform());
    OwnDirs {
        home: Some(home.to_path_buf()),
        config: aterm_types::dirs::resolve_aterm_config_dir(None, None, Some(home.as_os_str())),
        logs: aterm_types::dirs::resolve_logs_dir(None, platform()),
        // `aterm_uds::CONTROL_SOCKET_DIR_DEFAULT`: the state folder on macOS, `run` in it
        // elsewhere.
        control: state.clone().map(|s| {
            if cfg!(target_os = "macos") {
                s
            } else {
                s.join("run")
            }
        }),
        state,
    }
}

/// [`product_dirs`] with `$XDG_RUNTIME_DIR` set, as a Linux login sets it and a lab instance
/// does on a Mac: the control folder is `$XDG_RUNTIME_DIR/aterm` (`aterm_uds::
/// control_socket_dir`), outside the home and every other folder of aterm's.
fn runtime_dirs(home: &Path) -> OwnDirs {
    OwnDirs {
        control: Some(PathBuf::from("/run/user/1000/aterm")),
        ..product_dirs(home)
    }
}

fn spec_of(specs: &[StoreSpec], word: StoreWord) -> &StoreSpec {
    specs
        .iter()
        .find(|s| s.word == word)
        .unwrap_or_else(|| panic!("no {} store", word.word()))
}

#[test]
fn sys_disk_stores_cover_every_folder_aterm_writes_to() {
    let home = home();
    covers_every_folder(home, &product_dirs(home));
    // With a runtime folder the control folder, and the `image` verb's captures in it, lie
    // outside the home: it is a root of its own, so the walk that sizes the captures exists.
    let runtime = runtime_dirs(home);
    covers_every_folder(home, &runtime);
    let macos = cfg!(target_os = "macos");
    let images = spec_of(&specs(&runtime), StoreWord::Images).path.clone();
    assert_eq!(
        images,
        Path::new("/run/user/1000/aterm").join(crate::control_auth::IMAGES_DIR)
    );
    let roots = own_folders(&own_paths(&runtime, macos), macos);
    assert!(roots.contains(&PathBuf::from("/run/user/1000/aterm")));
    // NEGATIVE CONTROL: with the control folder no root, no root holds the captures, no walk
    // reads them and their line is unread for good.
    let without = own_folders(
        &own_paths(
            &OwnDirs {
                control: None,
                ..runtime.clone()
            },
            macos,
        ),
        macos,
    );
    assert!(!without.iter().any(|root| within(&images, root, macos)));
}

/// Every folder aterm writes to under `dirs` lies in a root of the aterm scope, each fixed word
/// has its store, and each bound is the one its store's code enforces, held as that code holds
/// it.
fn covers_every_folder(home: &Path, dirs: &OwnDirs) {
    let macos = cfg!(target_os = "macos");
    let roots = own_folders(&own_paths(dirs, macos), macos);
    let state = dirs.state.clone().expect("a state folder");
    let specs = specs(dirs);

    // Every folder a path function of aterm's own code names: the stores, the platform
    // folders, and the homes of what is kept outside them.
    let mut written: Vec<(String, PathBuf)> = specs
        .iter()
        .map(|s| (format!("store {}", s.word.word()), s.path.clone()))
        .collect();
    let mut name = |what: &str, path: PathBuf| written.push((what.to_owned(), path));
    name("state", state.clone());
    name("config", dirs.config.clone().expect("a config folder"));
    name("logs", dirs.logs.clone().expect("a logs folder"));
    name("control", dirs.control.clone().expect("a control folder"));
    name(
        "vault",
        aterm_secrets::client::Paths::locate_for_home(home).dir,
    );
    name("packages", atpkg::platform::default_prefix(home));
    name(
        "journal spool",
        aterm_db::alab::journal::Spool::under(&state)
            .dir()
            .to_path_buf(),
    );
    name("telemetry", aterm_telemetry::log::dir_in(&state));
    let harness = aterm_agent::harness::cli::harness_state_in(&state);
    name("harness state", harness.clone());
    for cli in Cli::ALL {
        let store = aterm_agent::harness::accounts::Store::at_for(&harness, cli, home);
        name("account logins", store.logins_dir());
    }
    name(
        "stores' history",
        state.join(crate::diskwalk_host::STORE_HISTORY_FILE),
    );
    // The folders the owner's measurement of 2026-11-14 found aterm writing to on a Mac.
    if macos {
        for rel in [
            "Library/Application Support/aterm",
            ".config/aterm",
            ".aterm",
            "Library/Logs/aterm",
            "Library/Caches/aterm",
            ".cache/aterm",
        ] {
            name("measured", home.join(rel));
        }
    }
    let under_a_root =
        |roots: &[PathBuf], path: &Path| roots.iter().any(|root| within(path, root, macos));
    for (what, path) in &written {
        assert!(
            under_a_root(&roots, path),
            "{what} ({}) is under no root of the aterm scope: {roots:?}",
            path.display()
        );
    }
    // A root the walker refuses by its path is counted by nobody: it may enter every one.
    let deny = Deny::for_home(home, &[], &[]);
    for root in &roots {
        assert_eq!(deny.denies(root), None, "{}", root.display());
    }

    // NEGATIVE CONTROL: the roots as they were (state, the build cache, the package prefix)
    // leave the vault, the logs and the two dot folders outside the count.
    let before: Vec<PathBuf> = own_folders(
        &[
            state.clone(),
            home.join("Library/Caches/aterm"),
            atpkg::platform::default_prefix(home),
        ],
        macos,
    );
    let outside: Vec<&str> = written
        .iter()
        .filter(|(_, path)| !under_a_root(&before, path))
        .map(|(what, _)| what.as_str())
        .collect();
    assert!(outside.contains(&"store vault-records"), "{outside:?}");
    assert!(outside.contains(&"account logins"), "{outside:?}");
    if macos {
        assert!(outside.contains(&"logs"), "{outside:?}");
    }
    // And a folder that is not aterm's is under none.
    assert!(!under_a_root(&roots, &home.join(".config/other")));
    assert!(!under_a_root(&roots, &home.join("Library/Logs")));

    // Every fixed word has its store, and the bounds are the ones the stores' code enforces.
    for word in StoreWord::ALL {
        assert!(specs.iter().any(|s| s.word == word), "{}", word.word());
    }
    let bound = |word| spec_of(&specs, word).bound;
    let files = |bytes: u64, at: AtBound| {
        Some(StoreBound {
            bytes,
            at,
            walked: true,
        })
    };
    assert_eq!(
        bound(StoreWord::VaultRecords),
        files(
            aterm_secrets::membership_log::MAX_RECORDS_STREAM as u64,
            AtBound::Refuses
        )
    );
    assert_eq!(
        bound(StoreWord::JournalOutbox),
        files(aterm_db::alab::journal::spool::MAX_SPOOL, AtBound::Refuses)
    );
    // The two that drop their oldest at the bound say so: neither warns at its steady state.
    let limits = aterm_telemetry::log::Limits::DEFAULT;
    assert_eq!(
        bound(StoreWord::Telemetry),
        files(
            limits.file_bytes * u64::from(limits.files),
            AtBound::Discards
        )
    );
    assert_eq!(
        bound(StoreWord::JournalHeard).map(|b| (b.at, b.walked)),
        Some((AtBound::Discards, true))
    );
    // A table's bound is the operation bytes its store refuses a write past (128 MiB), not
    // its journal's envelope on disk, and no walk reads what it counts.
    assert_eq!(
        bound(StoreWord::CommonsTable),
        Some(StoreBound {
            bytes: aterm_db::table::store::operation_bytes_bound(),
            at: AtBound::Refuses,
            walked: false,
        })
    );
    assert_eq!(aterm_db::table::store::operation_bytes_bound(), 128 << 20);
    let bounded: Vec<&str> = StoreWord::ALL
        .into_iter()
        .filter(|w| bound(*w).is_some())
        .map(StoreWord::word)
        .collect();
    assert_eq!(
        bounded,
        [
            "vault-records",
            "commons-table",
            "journal-heard",
            "journal-outbox",
            "telemetry"
        ]
    );
}

fn bytes(logical: u64, files: u64, growth_per_h: Option<i64>) -> StoreBytes {
    StoreBytes {
        alloc: logical.next_multiple_of(4096),
        logical,
        files,
        growth_per_h,
        fill_per_h: growth_per_h,
        n: None,
    }
}

/// [`bytes`] of the table the history numbers `n`.
fn table(n: u32, logical: u64, growth_per_h: Option<i64>) -> StoreBytes {
    StoreBytes {
        n: Some(n),
        ..bytes(logical, 5, growth_per_h)
    }
}

fn lines(facts: &[StoreFact]) -> Vec<String> {
    facts.iter().map(StoreFact::text).collect()
}

#[test]
fn sys_disk_stores_read_the_walkers_board_then_its_kept_history_and_name_no_table() {
    let home = home();
    let dirs = product_dirs(home);
    let specs: Vec<StoreSpec> = specs(&dirs)
        .into_iter()
        .filter(|s| {
            matches!(
                s.word,
                StoreWord::VaultRecords
                    | StoreWord::VaultServed
                    | StoreWord::CommonsTable
                    | StoreWord::JournalHeard
                    | StoreWord::JournalOutbox
            )
        })
        .collect();
    let path = |word| spec_of(&specs, word).path.clone();
    let now = Instant::now();
    let walked = now - Duration::from_secs(30);
    let unix_s = 1_794_700_000;
    let mut board = Published::default();
    let mut put = |word, seen| {
        board
            .stores
            .insert(path(word), StoreRead { at: walked, seen });
    };
    put(
        StoreWord::VaultRecords,
        StoreSeen::One(StoreBytes {
            alloc: 52_805_632,
            ..bytes(52_800_000, 2, Some(1_048_576))
        }),
    );
    // This machine serves no vault: the walk found nothing there.
    put(StoreWord::VaultServed, StoreSeen::Absent);
    put(
        StoreWord::CommonsTable,
        // In name order, as a walk lists them; `0f3a9c` came after `b7e2d1` and is numbered
        // after it.
        StoreSeen::Children(vec![
            ("0f3a9c".into(), table(2, 4_584_849, Some(0))),
            ("b7e2d1".into(), table(1, 13_568, None)),
        ]),
    );
    // No kept walk holds the journal's files; the history holds the outbox's last two sizes.
    let mut history = StoreHistory::default();
    let outbox = history_key(&path(StoreWord::JournalOutbox), None);
    for (ago, size) in [(4 * 3600, 2_048), (2 * 3600, 4_096)] {
        history.note(
            &outbox,
            StoreSample {
                at_s: unix_s - ago,
                alloc: size,
                logical: size - 100,
                files: 1,
            },
        );
    }
    let reads = Cell::new(0);
    let kept = || {
        reads.set(reads.get() + 1);
        Some(history.clone())
    };
    let state = if cfg!(target_os = "macos") {
        "Library/Application%20Support/aterm"
    } else {
        ".local/state/aterm"
    };
    let listed = facts(&specs, &board, Some(home), &kept, now, unix_s);
    assert_eq!(
        lines(&listed),
        [
            "store name=vault-records under=home path=.config/aterm/secrets/records \
             alloc_bytes=52805632 logical_bytes=52800000 files=2 bound_bytes=67108864 \
             at_bound=refuses used_pct=78 growth_bytes_per_h=1048576 full_in_s=49125 \
             walked_s=30"
                .to_owned(),
            // The tables stand in their numbers' order, each with the bound on its operations
            // and no share of it: a walk reads its files, not what it holds.
            "store name=commons-table n=1 under=home path=.config/aterm/secrets/commons/tables \
             alloc_bytes=16384 logical_bytes=13568 files=5 bound_bytes=134217728 \
             at_bound=refuses used_pct=unavailable(unread) \
             growth_bytes_per_h=unavailable(unread) full_in_s=unavailable(unread) walked_s=30"
                .to_owned(),
            "store name=commons-table n=2 under=home path=.config/aterm/secrets/commons/tables \
             alloc_bytes=4587520 logical_bytes=4584849 files=5 bound_bytes=134217728 \
             at_bound=refuses used_pct=unavailable(unread) growth_bytes_per_h=0 \
             full_in_s=unavailable(unread) walked_s=30"
                .to_owned(),
            // The archive drops its oldest days at its bound: it has no time to it.
            format!(
                "store name=journal-heard under=home path={state}/alab/heard \
                 alloc_bytes=unavailable(unread) logical_bytes=unavailable(unread) \
                 files=unavailable(unread) bound_bytes=67108864 at_bound=discards \
                 used_pct=unavailable(unread) growth_bytes_per_h=unavailable(unread) \
                 walked_s=unavailable(unread)"
            ),
            // 2,048 bytes in two hours; 4 MiB less 3,996 bytes at 1,024 an hour.
            format!(
                "store name=journal-outbox under=home path={state}/alab/outbox.jsonl \
                 alloc_bytes=4096 logical_bytes=3996 files=1 bound_bytes=4194304 \
                 at_bound=refuses used_pct=0 growth_bytes_per_h=1024 full_in_s={} walked_s=7200",
                (4_194_304u64 - 3_996) * 3600 / 1024
            ),
        ]
    );
    assert_eq!(
        reads.get(),
        1,
        "the history is read once, however many need it"
    );
    // No line names a table's folder, in the text or the JSON.
    for fact in &listed {
        let said = format!("{} {}", fact.text(), fact.json().to_text());
        assert!(
            !said.contains("0f3a9c") && !said.contains("b7e2d1"),
            "{said}"
        );
    }

    // A board that holds every store reads no history.
    let reads = Cell::new(0);
    let kept = || {
        reads.set(reads.get() + 1);
        None
    };
    for word in [StoreWord::JournalHeard, StoreWord::JournalOutbox] {
        board.stores.insert(
            path(word),
            StoreRead {
                at: walked,
                seen: StoreSeen::One(bytes(10, 1, None)),
            },
        );
    }
    let listed = facts(&specs, &board, Some(home), &kept, now, unix_s);
    assert_eq!(listed.len(), 5);
    assert_eq!(reads.get(), 0);

    // With neither a walk nor a history every figure is unread, and a size the history last
    // took more than seven days ago is no size.
    let none = facts(
        &specs,
        &Published::default(),
        Some(home),
        &|| None,
        now,
        unix_s,
    );
    assert!(
        none.iter()
            .all(|f| f.size.is_none() && f.growth_per_h.is_none())
    );
    assert_eq!(none.len(), 5, "one line a store, the tables' folder as one");
    let stale = facts(
        &specs,
        &Published::default(),
        Some(home),
        &|| Some(history.clone()),
        now,
        unix_s + 8 * 86_400,
    );
    assert!(
        stale
            .iter()
            .all(|f| f.size.is_none() && f.growth_per_h.is_none())
    );

    // The tables of the last walk the history holds, and none it measured only before that,
    // each by the number the history kept for it: `gone` held 1, so `aa` and `bb` are 2 and 3
    // and stay so now that `gone` is no longer listed.
    let tables = history_key(&path(StoreWord::CommonsTable), None);
    for (name, at_s) in [
        ("gone", unix_s - 9_000),
        ("bb", unix_s - 600),
        ("aa", unix_s - 600),
    ] {
        let key = format!("{tables}/{name}");
        history.note(
            &key,
            StoreSample {
                at_s,
                alloc: 4_096,
                logical: 1_000,
                files: 5,
            },
        );
        history.number(&key);
    }
    let kept = facts(
        &specs,
        &Published::default(),
        Some(home),
        &|| Some(history.clone()),
        now,
        unix_s,
    );
    let numbered: Vec<(Option<u32>, Option<u64>)> = kept
        .iter()
        .filter(|f| f.word == StoreWord::CommonsTable)
        .map(|f| (f.n, f.size.and_then(|s| s.age_s)))
        .collect();
    assert_eq!(numbered, [(Some(2), Some(600)), (Some(3), Some(600))]);
}

/// A file of `len` logical bytes at `path`, its folders made: a hole, so a test of a store near
/// its bound writes nothing of that size.
fn file_of(path: &Path, len: u64) {
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("mkdir");
    std::fs::File::create(path)
        .and_then(|file| file.set_len(len))
        .expect("a scratch file");
}

/// The survey's fullest store is read from the stores' own files as they are now, with no walk
/// and no walker: the fullest of those that refuse writes at a bound over their files. A store
/// that drops its oldest at its bound, a table (whose bound no file size reads) and a store
/// that is not there are never it.
/// NEGATIVE CONTROL: read from the walker's board, as `sys disk`'s lines are, a Mac nobody has
/// asked `sys disk` of holds no reading: its survey says `store_worst=-` while its vault log
/// stands at 83 % of the bound it refuses writes at.
#[test]
fn sys_disk_stores_fullest_refusing_store_is_sized_now_for_the_survey_with_no_walk() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    let dirs = product_dirs(&home);
    let specs = specs(&dirs);
    let path = |word| spec_of(&specs, word).path.clone();
    let worst = || worst_of(&specs, &sized_now).map(|w| (w.name, w.pct));

    // Nothing kept yet: no store, no figure.
    assert_eq!(worst(), None);

    // This Mac's records log on 2026-11-15 (56,060,292 bytes of 64 MiB) beside its layout file,
    // an outbox a quarter full, and two rings at their bounds.
    let records = path(StoreWord::VaultRecords);
    file_of(&records.join("current"), 56_060_292);
    file_of(&records.join("layout"), 234);
    file_of(&path(StoreWord::JournalOutbox), 1 << 20);
    file_of(
        &path(StoreWord::JournalHeard).join("2026-11-14"),
        66_000_000,
    );
    for n in 0..4 {
        file_of(
            &path(StoreWord::Telemetry).join(format!("rows.{n}.log")),
            4 << 20,
        );
    }
    file_of(
        &path(StoreWord::CommonsTable).join("0f3a9c/table.astream"),
        200 << 20,
    );
    assert_eq!(
        sized_now(&records).map(|s| (s.logical, s.files)),
        Some((56_060_526, 2))
    );
    assert_eq!(worst(), Some(("vault-records".to_owned(), 83)));
    // No walk read any of it: the board, which `sys disk`'s lines read, holds nothing.
    let lines = facts(
        &specs,
        &Published::default(),
        Some(&home),
        &|| None,
        Instant::now(),
        0,
    );
    assert!(lines.iter().all(|f| f.size.is_none()));

    // The outbox past the log's share is the one named; past its own bound it says so.
    file_of(&path(StoreWord::JournalOutbox), (4 << 20) - 4096);
    assert_eq!(worst(), Some(("journal-outbox".to_owned(), 99)));
    file_of(&path(StoreWord::JournalOutbox), (4 << 20) + (1 << 18));
    assert_eq!(worst(), Some(("journal-outbox".to_owned(), 106)));

    // A store that is no flat folder is not sized by a listing: it is unread here, never
    // counted short, and the other store is the one named.
    std::fs::remove_file(path(StoreWord::JournalOutbox)).expect("remove");
    std::fs::create_dir(records.join("older")).expect("mkdir");
    assert_eq!(sized_now(&records), None);
    assert_eq!(worst(), None);
    file_of(&path(StoreWord::JournalOutbox), 1 << 20);
    assert_eq!(worst(), Some(("journal-outbox".to_owned(), 25)));
}
