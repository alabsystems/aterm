// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::frame_asks_for_test as frame_asks;
use crate::update_words::LiveHealth as _;
use aterm_messages::{LogRecord, LogState, Retired, Severity, tags};

fn warn(title: &str) -> Message {
    Message::new(tags::CONFIG, Severity::Warn, title).line("why")
}

/// The zones the pinned Settings ▸ Messages tests say their times in: 9 h 17 min east of UTC
/// and 10 h 43 min west of it. No zone in the tz database is at either offset in 2026, and the
/// host's zone can be at most one of them, so a page that reads the host's zone in place of the
/// pinned one fails these tests in at least one of them on every machine.
const PINNED_ZONES_S: [i64; 2] = [9 * 3600 + 17 * 60, -(10 * 3600 + 43 * 60)];
/// The midnight that begins 2026-10-29 in UTC, unix ms.
const UTC_MIDNIGHT_MS: i64 = 1_793_232_000_000;
const MIN_MS: u64 = 60_000;
/// How old the record is at each of the [`words_cases`] instants.
const RECORD_AGE_MS: u64 = 190 * MIN_MS;

/// The instants the page is read at in a zone `zone_s` seconds east of UTC, each with how long
/// until the words of a record [`RECORD_AGE_MS`] old move. At 15:00 its `3 h ago` becomes
/// `4 h ago` 50 min on; at 23:59:30 today's `3 h ago` becomes yesterday's clock time at
/// midnight, 30 s on; at 00:30 the record is yesterday's, and its spoken `3 hours ago` becomes
/// `4 hours ago` 50 min on.
fn words_cases(zone_s: i64) -> [(&'static str, u64, std::time::Duration); 3] {
    // The midnight that begins 2026-10-29 in the zone.
    let midnight = u64::try_from(UTC_MIDNIGHT_MS - zone_s * 1000).expect("after the epoch");
    [
        (
            "15:00",
            midnight - 9 * 60 * MIN_MS,
            std::time::Duration::from_secs(50 * 60),
        ),
        (
            "23:59:30",
            midnight - 30_000,
            std::time::Duration::from_secs(30),
        ),
        (
            "00:30",
            midnight + 30 * MIN_MS,
            std::time::Duration::from_secs(50 * 60),
        ),
    ]
}

/// The page's clock standing still at `unix_ms` in a zone `zone_s` seconds east of UTC.
const fn pinned_wall(unix_ms: u64, zone_s: i64) -> crate::messages_host::WallClock {
    crate::messages_host::WallClock::Pinned {
        unix_ms,
        offset_s: zone_s,
    }
}

use crate::toolchain_words::held_row;

fn settings_view(app: &App, wid: WindowId) -> Option<&crate::native_settings::SettingsViewState> {
    let (_, view) = app.active_native_view(wid)?;
    match app.native_runtime.view_state(view)? {
        crate::native_app::AppViewState::Settings(state) => Some(state),
        _ => None,
    }
}

#[test]
fn native_centers_share_one_confirmation_incarnation() {
    let now = Instant::now();
    let mut first = crate::messages_host::new_center(aterm_messages::MessageLog::empty(), now);
    let mut next = crate::messages_host::new_center(aterm_messages::MessageLog::empty(), now);
    let req = "later incarnation Owner action -- only you have access -- Resolve this in settings";
    let stamp = WallStamp { unix_ms: 0 };
    let post = |center: &mut aterm_messages::MessageCenter| {
        wire::apply(
            center,
            &mut wire::WireGate::default(),
            wire::NoticeRequest::parse(req).unwrap(),
            stamp,
            now,
        )
    };
    post(&mut first);
    post(&mut next);
    let read = |center: &aterm_messages::MessageCenter| {
        wire::message_rows(
            center,
            &wire::parse_read_args("live need=later").unwrap(),
            0,
            &str::to_string,
        )
        .into_iter()
        .next()
        .unwrap()
    };
    let parent = read(&first);
    let successor = read(&next);
    let revision = |row: &str| {
        row.split_whitespace()
            .find_map(|word| word.strip_prefix("rev="))
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    let old = revision(&parent);
    if old == u64::MAX {
        assert_eq!(
            revision(&successor),
            u64::MAX,
            "unavailable entropy stays disabled"
        );
    } else {
        assert_ne!(
            revision(&successor),
            old,
            "opening another center must not reseed or reset"
        );
    }
    let applied = wire::apply(
        &mut next,
        &mut wire::WireGate::default(),
        wire::NoticeRequest::parse(&format!("settle incarnation 1 {old}")).unwrap(),
        stamp,
        now,
    );
    assert_eq!(applied.reply, "OK settled=- how=gone");
    assert!(!next.digest().is_empty());
}

#[test]
fn the_pre_app_inbox_drains_into_the_center_with_its_own_stamp() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    let _ = crate::message_inbox::take_queued();
    let mut app = App::headless_for_test();
    crate::message_inbox::queue_message(warn("queued before App"));
    let before = wall_stamp_now();
    app.drain_message_inbox();
    let live = app
        .messages
        .live_rows()
        .find(|l| l.msg.title == "queued before App")
        .expect("posted from the inbox");
    assert!(
        live.stamp.unix_ms <= before.unix_ms,
        "stamped at queue time, not drain time"
    );
    assert_eq!(app.messages.log().len(), 1);
    assert_eq!(
        app.messages.log().pending_len(),
        0,
        "drained to the (absent) writer, never left to fill the queue"
    );
}

#[test]
fn torn_fabric_state_set_aside_is_one_exceptions_entry_at_the_next_park() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    use crate::fabric::Registry as _;
    let _ = crate::message_inbox::take_queued();
    let _ = crate::message_inbox::take_queued_exceptions();
    let mut app = App::headless_for_test();
    let entry = crate::message_reporters::torn_state_set_aside_entry(3, true);
    let key = entry.key();
    assert_eq!(key, "fabric.state.torn");
    assert_eq!(
        entry.family(),
        aterm_messages::exceptions::Family::FabricState
    );
    entry.message().expect("the words pass the rule");
    let store = crate::session_store::new_store();
    store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .torn_state_set_aside(3, true);
    assert!(!app.owes_exception(&key), "queued, not yet raised");
    app.drain_message_inbox();
    assert!(app.owes_exception(&key), "raised at the park");
    assert_eq!(
        app.messages.live_rows().count(),
        0,
        "a fact for later, never a row on the glass"
    );
    store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .torn_state_set_aside(1, true);
    app.drain_message_inbox();
    assert_eq!(
        app.messages
            .digest_keys_under("fabric.state.")
            .iter()
            .filter(|k| **k == key)
            .count(),
        1,
        "one entry however many cuts"
    );
}

#[test]
fn torn_fabric_state_ends_once_the_quarantine_holds_none_of_it() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    let _ = crate::message_inbox::take_queued();
    let _ = crate::message_inbox::take_queued_exceptions();
    let key = crate::message_reporters::torn_state_key();
    assert_eq!(
        key,
        crate::message_reporters::torn_state_set_aside_entry(3, true).key(),
        "the key it ends is the key it was filed under"
    );
    let store = crate::session_store::new_store();
    let say = |f: &dyn Fn(&dyn crate::fabric::Registry)| {
        f(&*store.read().unwrap_or_else(|p| p.into_inner()));
    };

    let mut app = App::headless_for_test();
    say(&|r| r.torn_state_set_aside(3, true));
    app.drain_message_inbox();
    assert!(app.owes_exception(&key), "raised at the park");
    say(&|r| r.torn_state_held(3));
    app.drain_message_inbox();
    assert!(app.owes_exception(&key), "still held: the entry stands");
    say(&|r| r.torn_state_held(0));
    assert!(app.owes_exception(&key), "queued, not yet ended");
    app.drain_message_inbox();
    assert!(!app.owes_exception(&key), "ended at the park");
    assert!(app.messages.digest_keys_under("fabric.state.").is_empty());
    assert_eq!(app.messages.live_rows().count(), 0, "never a row");

    say(&|r| r.torn_state_set_aside(1, true));
    say(&|r| r.torn_state_held(0));
    app.drain_message_inbox();
    assert!(
        !app.owes_exception(&key),
        "the empty quarantine was the last word"
    );
    say(&|r| r.torn_state_held(0));
    say(&|r| r.torn_state_set_aside(2, true));
    app.drain_message_inbox();
    assert!(app.owes_exception(&key), "the set-aside was the last word");

    for emptied in [false, true] {
        let mut app = App::headless_for_test();
        say(&|r| r.torn_state_set_aside(2, true));
        app.drain_message_inbox();
        let id = app
            .messages
            .digest()
            .entries
            .iter()
            .find(|e| e.key == key)
            .expect("the entry is open")
            .id;
        assert!(app.messages.file_digest_one(
            id,
            crate::messages_host::wall_stamp_now(),
            std::time::Instant::now()
        ));
        assert!(!app.owes_exception(&key), "dismissed");
        if emptied {
            say(&|r| r.torn_state_held(0));
            app.drain_message_inbox();
        }
        say(&|r| r.torn_state_set_aside(1, true));
        app.drain_message_inbox();
        assert_eq!(
            app.owes_exception(&key),
            emptied,
            "emptied={emptied}: the next power cut's set-aside"
        );
    }
}

#[test]
fn torn_fabric_state_promises_an_end_only_where_its_bridge_tells_one() {
    let says = |told: bool| {
        let entry = crate::message_reporters::torn_state_set_aside_entry(2, told);
        assert_eq!(entry.key(), crate::message_reporters::torn_state_key());
        entry.message().expect("the words pass the rule").detail
    };
    let ends = |detail: &[String]| detail.iter().any(|l| l.contains("This ends once"));
    let told = says(true);
    assert!(ends(&told), "{told:?}");
    let older = says(false);
    assert!(!ends(&older), "an older bridge never ends it: {older:?}");
    assert!(
        older.iter().any(|l| l.contains("dismiss this")),
        "{older:?}"
    );
}

fn fell_back(dir: &str, fault: crate::spawn_folder::Fault) {
    crate::spawn_folder::StartFolder {
        cwd: Some("/Users//me".into()),
        fell_back: Some((dir.into(), fault)),
    }
    .say();
}

#[test]
fn folders_that_were_not_found_fold_into_one_row_while_it_is_up() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    use crate::spawn_folder::Fault;
    let _ = crate::message_inbox::take_queued();
    let _ = crate::spawn_folder::take_noted();
    let mut app = App::headless_for_test();
    let key = crate::message_reporters::KEY_FOLDER_NOT_FOUND;
    let rows = |app: &App, key: &str| {
        app.messages
            .live_rows()
            .filter(|l| l.msg.key.as_deref() == Some(key))
            .count()
    };
    fell_back("/work/a", Fault::Missing);
    fell_back("/work/b", Fault::Missing);
    app.drain_message_inbox();
    assert_eq!(rows(&app, key), 1, "one row for the restore");
    let live = app.messages.live_by_key(key).expect("the row");
    assert_eq!(&live.msg.detail[1..], ["/work/a", "/work/b"]);

    fell_back("/work/c", Fault::Missing);
    fell_back("/srv/locked", Fault::Shut);
    app.drain_message_inbox();
    assert_eq!(rows(&app, key), 1, "still one row");
    let live = app.messages.live_by_key(key).expect("the row");
    assert_eq!(&live.msg.detail[1..], ["/work/a", "/work/b", "/work/c"]);
    let shut = crate::message_reporters::KEY_FOLDER_NOT_OPENED;
    assert_eq!(rows(&app, shut), 1, "a folder not entered: its own row");

    let id = app.messages.live_by_key(key).expect("the row").id;
    assert!(app.messages.dismiss(id, Instant::now()));
    fell_back("/work/d", Fault::Missing);
    app.drain_message_inbox();
    let fresh = &app
        .messages
        .live_by_key(key)
        .expect("a fresh row")
        .msg
        .detail;
    assert!(
        fresh.len() == 1 && fresh[0].ends_with("/work/d"),
        "a fresh row names only the new folder: {fresh:?}"
    );
}

#[test]
fn a_carried_folder_row_keeps_its_folders_when_the_next_one_joins() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    use crate::spawn_folder::{Fault, Named};
    let _ = crate::message_inbox::take_queued();
    let _ = crate::spawn_folder::take_noted();
    let key = crate::message_reporters::KEY_FOLDER_NOT_FOUND;
    let mut app = App::headless_for_test();
    let carried = crate::message_reporters::folder_row(
        Fault::Missing,
        &Named {
            dirs: vec!["/work/a".into(), "/work/b".into()],
            unnamed: 0,
        },
    );
    app.post_unsynced(carried, wall_stamp_now());
    fell_back("/work/c", Fault::Missing);
    app.drain_message_inbox();
    let live = app.messages.live_by_key(key).expect("the row");
    assert_eq!(&live.msg.detail[1..], ["/work/a", "/work/b", "/work/c"]);
}

#[test]
fn a_config_reload_resolves_the_old_config_rows_then_posts_the_new() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings};
    let mut app = App::headless_for_test();
    let mut launch = ConfigWarnings::default();
    launch.push(
        ConfigFamily::Keybindings,
        "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
    );
    launch.push(
        ConfigFamily::IgnoredKeys,
        "config line 3: unknown key \"windw_padding\"".into(),
    );
    app.replace_config_messages(launch.into_messages());
    let other = app.post_message(warn("a row keyed elsewhere").key("privacy.fda"));
    assert_eq!(app.messages.live_rows().count(), 3);
    let old_keys = app
        .messages
        .live_by_key("config.keybindings")
        .map(|l| l.id)
        .expect("the keybinding row");
    let old_typo = app
        .messages
        .live_by_key("config.ignored-keys")
        .map(|l| l.id)
        .expect("the typo's row");

    let mut reload = ConfigWarnings::default();
    reload.push(
        ConfigFamily::IgnoredKeys,
        "config line 3: unknown key \"windw_padding\"".into(),
    );
    app.replace_config_messages(reload.into_messages());
    assert!(
        app.messages.live(old_keys).is_none(),
        "the old row is resolved"
    );
    assert!(app.messages.live_by_key("config.keybindings").is_none());
    let typo = app
        .messages
        .live_by_key("config.ignored-keys")
        .expect("the typo's row stands");
    assert_eq!(typo.id, old_typo, "the same row, not a new one");
    assert!(typo.msg.detail.iter().any(|l| l.contains("windw_padding")));
    assert!(
        app.messages.live(other).is_some(),
        "other keys are untouched"
    );
    assert_eq!(app.messages.live_rows().count(), 2);
}

#[test]
fn a_repeated_config_warning_is_a_record_once_its_row_left_the_glass() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings};
    let typo = |line: u32, key: &str| {
        let mut warns = ConfigWarnings::default();
        warns.push(
            ConfigFamily::IgnoredKeys,
            format!("config line {line}: unknown key \"{key}\""),
        );
        warns.into_messages()
    };
    let key = ConfigFamily::IgnoredKeys.key();
    let mut app = App::headless_for_test();
    app.replace_config_messages(typo(3, "windw_padding"));
    let first = app.messages.live_by_key(key).map(|l| l.id).expect("a row");
    assert!(app.messages.mark_seen(first, Instant::now()));
    app.sync_messages();
    assert!(app.messages.live_by_key(key).is_none(), "read, and folded");
    let records = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some(key))
            .count()
    };
    let before = records(&app);
    for _ in 0..3 {
        app.replace_config_messages(typo(3, "windw_padding"));
        assert!(
            app.messages.live_by_key(key).is_none(),
            "a repeat never comes back to the glass"
        );
    }
    assert_eq!(records(&app), before + 3, "…and the log keeps each one");
    assert_eq!(app.messages.wanted_rows(), 0, "no row is revealed for it");
    app.replace_config_messages(typo(5, "colums"));
    let changed = app.messages.live_by_key(key).expect("a new typo's row");
    assert!(changed.msg.detail.iter().any(|d| d.contains("colums")));
    app.replace_config_messages(Vec::new());
    assert!(app.messages.live_by_key(key).is_none(), "fixed");
    app.replace_config_messages(typo(5, "colums"));
    let again = app
        .messages
        .live_by_key(key)
        .map(|l| l.id)
        .expect("a problem that came back after its fix is raised again");
    assert!(app.messages.mark_seen(again, Instant::now()));
    app.sync_messages();
    app.replace_config_messages(typo(5, "colums"));
    assert!(
        app.messages.live_by_key(key).is_none(),
        "a repeat: a record"
    );
    app.replace_config_messages(Vec::new());
    app.replace_config_messages(typo(5, "colums"));
    assert!(
        app.messages.live_by_key(key).is_some(),
        "fixed while off the glass, then broken again: raised"
    );
}

#[test]
fn a_content_equal_reload_resolves_only_the_families_it_re_derived() {
    use crate::message_reporters::{
        ConfigFamily, ConfigWarnings, KEY_LAUNCH_LOAD, launch_load_failure,
    };
    let mut app = App::headless_for_test();
    let mut launch = ConfigWarnings::default();
    launch.push(
        ConfigFamily::Keybindings,
        "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
    );
    launch.push(
        ConfigFamily::IgnoredKeys,
        "config line 3: unknown key \"windw_padding\"".into(),
    );
    app.replace_config_messages(launch.into_messages());
    let load = app.post_message(launch_load_failure(
        "aterm.toml could not be read at launch (permission denied) \u{2014} every setting \
         is running at its default.",
    ));
    let chord = app
        .messages
        .live_by_key(ConfigFamily::Keybindings.key())
        .map(|l| l.id)
        .expect("the chord's row");
    let typo = app
        .messages
        .live_by_key(ConfigFamily::IgnoredKeys.key())
        .map(|l| l.id)
        .expect("the typo's row");
    let keys = [
        ConfigFamily::IgnoredKeys.key(),
        ConfigFamily::UnacceptedValues.key(),
        KEY_LAUNCH_LOAD,
    ];

    app.replace_config_messages_keyed(&keys, Vec::new());
    assert!(
        app.messages.live(typo).is_none(),
        "the typo's row is resolved"
    );
    assert!(
        app.messages.live(load).is_none(),
        "a file that loads answers the launch failure"
    );
    assert!(
        app.messages.live(chord).is_some(),
        "the chord's row was not re-derived, so it stands"
    );
    assert_eq!(app.messages.live_rows().count(), 1);
    let rec = app.messages.log().get(typo).expect("logged");
    assert_eq!(
        rec.retired(),
        Some(&aterm_messages::Retired::Resolved(Outcome::Ok)),
        "the log says the typo was fixed: {rec:?}"
    );
}

#[test]
fn a_config_family_whose_words_changed_is_not_logged_fixed() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings};
    use aterm_messages::Retired;
    let chords = |names: &[&str]| {
        let mut warns = ConfigWarnings::default();
        for name in names {
            warns.push(
                ConfigFamily::Keybindings,
                format!("config keybindings: skipping \"{name}\": unknown action \"foo\""),
            );
        }
        warns.into_messages()
    };
    let key = ConfigFamily::Keybindings.key();
    let mut app = App::headless_for_test();
    app.replace_config_messages(chords(&["ctrl+x", "ctrl+y"]));
    let two = app.messages.live_by_key(key).map(|l| l.id).expect("a row");

    app.replace_config_messages(chords(&["ctrl+y"]));
    let one = app
        .messages
        .live_by_key(key)
        .expect("still a problem: a row")
        .id;
    assert_ne!(one, two, "the changed words are a new row");
    let rec = app.messages.log().get(two).expect("logged");
    assert!(
        matches!(rec.retired(), Some(Retired::Superseded { .. })),
        "the problem became another, it was not fixed: {rec:?}"
    );
    assert_eq!(rec.severity, Severity::Warn, "its ⚠, never a fix: {rec:?}");
    assert_eq!(
        app.messages.wanted_rows(),
        1,
        "one row, in the old one's place"
    );
    let page = app.messages_state();
    assert_eq!(page.standing_for(two.raw()), Some(one.raw()));
    assert!(
        page.entry(one.raw()).is_some_and(|entry| entry.alarm()),
        "the problem stays in Problems, as the new row"
    );

    app.replace_config_messages(Vec::new());
    assert!(app.messages.live_by_key(key).is_none());
    let rec = app.messages.log().get(one).expect("logged");
    assert_eq!(rec.retired(), Some(&Retired::Resolved(Outcome::Ok)));
    assert_eq!(rec.severity, Severity::Success, "{rec:?}");

    let typos = |keys: &[&str]| {
        let mut warns = ConfigWarnings::default();
        for (line, k) in keys.iter().enumerate() {
            warns.push(
                ConfigFamily::IgnoredKeys,
                format!("config line {}: unknown key \"{k}\"", line + 3),
            );
        }
        warns.into_messages()
    };
    let key = ConfigFamily::IgnoredKeys.key();
    app.replace_config_messages(typos(&["windw_padding"]));
    let first = app.messages.live_by_key(key).map(|l| l.id).expect("a row");
    app.replace_config_messages(typos(&["windw_padding", "colums"]));
    let rec = app.messages.log().get(first).expect("logged");
    assert_eq!(
        rec.severity,
        Severity::Warn,
        "a typo still there is not logged fixed: {rec:?}"
    );
    assert!(app.messages.live_by_key(key).is_some());
}

#[test]
fn a_retired_spelling_reaches_the_log_and_never_the_glass() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings};
    let mut app = App::headless_for_test();
    let text = "windw_padding = 20\n[packages]\nauto_update = false\n";
    for _ in 0..2 {
        let mut warns = ConfigWarnings::default();
        crate::app_config::collect_key_notices(&mut warns, text);
        app.replace_config_messages_keyed(
            &[
                ConfigFamily::IgnoredKeys.key(),
                ConfigFamily::RetiredKeys.key(),
            ],
            warns.into_messages(),
        );
    }
    assert!(
        app.messages
            .live_by_key(ConfigFamily::RetiredKeys.key())
            .is_none(),
        "a record is never a row"
    );
    assert!(
        app.messages
            .live_by_key(ConfigFamily::IgnoredKeys.key())
            .is_some_and(|l| l.msg.detail.iter().any(|d| d.contains("windw_padding"))),
        "the typo keeps its row"
    );
    assert_eq!(app.messages.live_rows().count(), 1);
    assert_eq!(app.messages.wanted_rows(), 1);
    let records = app
        .messages
        .log()
        .records()
        .filter(|r| r.key.as_deref() == Some(ConfigFamily::RetiredKeys.key()))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2, "one record per launch or reload");
    assert!(records.iter().all(|r| !r.is_live()));
}

#[test]
fn the_final_snapshot_leaves_a_held_row_alone() {
    let mut app = App::headless_for_test();
    let id = app.post_message(held_row(&toolchain_words::installed("claude, codex")));
    {
        let live = app.messages.live(id).expect("held live");
        assert!(live.msg.hold.is_held());
        assert_eq!(live.msg.meter, None, "no meter to complete");
        assert_eq!(live.revision, 0);
    }
    let file = atpkg::progress::ProgressFile {
        v: atpkg::progress::PROGRESS_VERSION,
        pid: None,
        pass: "net".into(),
        started_unix: 1_700_000_000,
        generation: None,
        heartbeat_unix: 1_700_000_100,
        overall: atpkg::progress::Overall {
            programs_done: 10,
            programs_total: 10,
            bytes_done: 1_200_000_000,
            bytes_total: 1_200_000_000,
        },
        queue: Vec::new(),
        programs: std::collections::BTreeMap::new(),
        ended_unix: Some(1_700_000_100),
    };
    for first_run in [false, true] {
        app.apply_toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: file.clone(),
                running: false,
            }),
            first_run,
        );
        app.apply_toolchain_snapshot(None, first_run);
    }
    app.toolchain_pass_ended(true);
    let live = app.messages.live(id).expect("still live, still held");
    assert_eq!(live.revision, 0, "not restated");
    assert_eq!(live.msg.meter, None, "no meter, no 100%");
    assert_eq!(app.messages.log().len(), 1, "the read posted nothing");
}

#[test]
fn show_tab_goes_to_the_named_window_not_the_pressing_one() {
    let mut app = App::headless_for_test();
    let a = WindowId(0);
    let target = app.next_session_id;
    app.push_stub_tab(a, crate::stub_session(target));
    let sid = {
        let mut store = app.store.write().unwrap();
        if store.by_local(target).is_none() {
            store.register(crate::session_store::test_handle(target));
        }
        store.by_local(target).unwrap().sid.as_str().to_string()
    };
    app.switch_tab_in(a, 0);
    let b = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    app.push_stub_tab(b, crate::stub_session(app.next_session_id));
    app.switch_tab_in(b, 0);
    let active = |app: &App, w: WindowId| app.windows[&w].tab_set.active_index();
    assert_eq!((active(&app, a), active(&app, b)), (Some(0), Some(0)));
    let id = app.post_message(warn("Typing slowed by 'yes' (tab 2)"));
    let show = Intent::ShowSession {
        tab: 2,
        window: a.0,
        session: sid.clone(),
    };
    assert!(app.perform_intent(b, id, show.clone()));
    assert_eq!(active(&app, a), Some(1), "the named window shows tab 2");
    assert_eq!(active(&app, b), Some(0), "the pressing window is untouched");
    app.move_tab(a, 1, 0);
    app.switch_tab_in(a, 1);
    assert!(app.perform_intent(b, id, show.clone()));
    assert_eq!(
        active(&app, a),
        Some(0),
        "the target moved, its identity did not"
    );
    assert_eq!(app.focused_session_id(a), Some(target));
    app.move_tab(a, 0, 1);
    app.close_tab_at(a, 0);
    assert_eq!(app.windows[&a].tab_set.len(), 1);
    assert!(app.perform_intent(b, id, show.clone()));
    assert_eq!(active(&app, a), Some(0));
    app.push_stub_tab(a, crate::stub_session(app.next_session_id));
    app.close_tab_at(a, 0);
    let before = active(&app, a);
    assert!(!app.perform_intent(b, id, show));
    assert_eq!(active(&app, a), before, "never select a replacement");
    assert!(!app.perform_intent(
        b,
        id,
        Intent::ShowSession {
            tab: 1,
            window: a.0,
            session: "missing".into()
        }
    ));
    let gone = app.post_message(warn("gone"));
    assert!(!app.perform_intent(
        b,
        gone,
        Intent::ShowTab {
            tab: 3,
            window: a.0
        }
    ));
    let gone = app.post_message(warn("gone window"));
    assert!(!app.perform_intent(a, gone, Intent::ShowTab { tab: 1, window: 99 }));
    let legacy = Intent::decode("show-tab:1:0").expect("readable legacy intent");
    assert!(!app.perform_intent(b, id, legacy));
    assert_eq!(active(&app, a), before);
}

#[test]
fn a_details_press_marks_the_row_seen_and_folds_a_held_one() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let id = app.post_message(warn("read me"));
    assert!(app.perform_intent(wid, id, Intent::Details));
    assert!(
        app.messages.live(id).is_none(),
        "a held row folds once read"
    );
    assert_eq!(
        app.messages.log().get(id).unwrap().state,
        LogState::Retired(Retired::Folded)
    );
    let view = settings_view(&app, wid).expect("Settings fronts the pressed window");
    assert_eq!(view.route, SettingsRoute::Messages);
    assert_eq!(view.messages_selected, Some(id.raw()));
    assert_eq!(
        app.messages_published_revision,
        app.messages.revision(),
        "…at the center's revision AFTER the fold, so the entry reads as folded"
    );
    let standing = app.post_message(
        Message::new(tags::UPDATE, Severity::Warn, "checks failing").hold(Hold::STANDING),
    );
    assert!(app.perform_intent(wid, standing, Intent::Details));
    assert!(
        app.messages.live(standing).is_none(),
        "a standing row folds once read"
    );
    assert_eq!(
        app.messages.log().get(standing).unwrap().state,
        LogState::Retired(Retired::Folded)
    );
    assert!(app.open_messages_entry(wid, None));
    let view = settings_view(&app, wid).unwrap();
    assert_eq!(view.route, SettingsRoute::Messages);
    assert_eq!(
        view.messages_selected,
        Some(standing.raw()),
        "a plain open leaves the page as it was"
    );
    assert!(!app.open_messages_entry(WindowId(77), Some(standing)));
}

#[test]
fn the_projection_reads_the_hosts_facts() {
    let mut app = App::headless_for_test();
    let open_log = |app: &mut App, path: String| {
        app.post_message(
            Message::new(tags::CRASH, Severity::Error, format!("crash log at {path}"))
                .action(Intent::OpenPath { path }),
        )
    };
    let gone = open_log(&mut app, "/definitely/not/there.log".into());
    let here = std::env::current_exe().expect("the test binary");
    let present = open_log(&mut app, here.to_string_lossy().into_owned());
    let staged = app.post_message(update_words::staged(
        "0.91.0",
        1234,
        Some(update_words::ApplyPosture::ManualByConfig),
    ));
    let state = app.messages_state();
    assert_eq!(state.revision, app.messages.revision());
    let entry = |id: MessageId| state.entry(id.raw()).expect("in the projection");
    assert!(
        !entry(gone).actions[0].still_actionable,
        "no file, no press"
    );
    assert!(
        entry(present).actions[0].still_actionable,
        "CONTROL: a file that is there is pressable"
    );
    let s = entry(staged);
    let apply = s
        .actions
        .iter()
        .find(|a| a.label == "Install now")
        .expect("a manually applied staged row offers Install now");
    assert!(!apply.still_actionable, "nothing is staged in a test App");
    assert!(
        s.actions
            .iter()
            .filter(|a| a.label != "Install now")
            .all(|a| a.still_actionable),
        "every other capsule is a navigation: {:?}",
        s.actions
    );
}

#[test]
fn the_pages_press_acts_through_the_engine_and_words_the_outcome() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let nav = app.post_message(
        Message::new(tags::TOOLCHAIN, Severity::Success, "ALab tools installed").action(
            Intent::OpenSettings {
                route: "/packages".into(),
            },
        ),
    );
    assert!(app.resolve_message(nav, Outcome::Ok));
    assert!(matches!(
        app.perform_message_act(wid, nav.raw(), 0),
        MessageActOutcome::Performed { .. }
    ));
    assert_eq!(
        settings_view(&app, wid).map(|v| v.route),
        Some(SettingsRoute::Packages)
    );
    assert_eq!(
        app.perform_message_act(wid, nav.raw(), 1),
        MessageActOutcome::Gone
    );
    assert_eq!(
        app.perform_message_act(wid, 9_999, 0),
        MessageActOutcome::Gone
    );
    let crash = app.post_message(
        Message::new(
            tags::CRASH,
            Severity::Error,
            "aterm closed unexpectedly last time",
        )
        .action(Intent::OpenPath {
            path: "/definitely/not/there.log".into(),
        }),
    );
    assert!(matches!(
        app.perform_message_act(wid, crash.raw(), 0),
        MessageActOutcome::Refused { .. }
    ));
}

#[test]
fn a_transient_status_line_is_cleared_about_five_seconds_on() {
    use crate::app_native::{
        MESSAGES_FEEDBACK_WAIT as WAIT, MESSAGES_PUBLISH_MIN_GAP as GAP,
        MESSAGES_PUBLISH_TICK as TICK,
    };
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().window_on_screen_for_test = true;
    app.post_message(warn("a record the page dates"));
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    let opened = app
        .messages_last_publish
        .expect("opening Settings publishes");
    assert_eq!(app.messages_publish_due_at(), Some(opened + TICK));
    fn view_mut(app: &mut App, wid: WindowId) -> &mut crate::native_settings::SettingsViewState {
        let (_, view) = app.active_native_view(wid).expect("Settings fronts it");
        match app.native_runtime.view_state_mut(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => state,
            _ => unreachable!(),
        }
    }
    view_mut(&mut app, wid).show_transient_status_for_test("Copied");
    assert_eq!(app.messages_publish_due_at(), Some(opened + GAP));
    app.publish_native_messages_state_if_due(opened + GAP);
    let stamped = app.messages_last_publish.unwrap();
    assert_ne!(stamped, opened);
    assert_eq!(settings_view(&app, wid).unwrap().status_clock(), Some(true));
    assert!(settings_view(&app, wid).unwrap().feedback.is_some());
    assert_eq!(app.messages_publish_due_at(), Some(stamped + WAIT));
    assert!(app.messages_deadline().is_some_and(|d| d <= stamped + WAIT));
    app.publish_native_messages_state_if_due(stamped + WAIT - std::time::Duration::from_millis(1));
    assert_eq!(app.messages_last_publish, Some(stamped), "not yet");
    view_mut(&mut app, wid)
        .age_transient_status_for_test(crate::native_settings::MESSAGES_FEEDBACK_MS);
    app.publish_native_messages_state_if_due(stamped + WAIT);
    let cleared = app.messages_last_publish.unwrap();
    assert_ne!(cleared, stamped);
    let view = settings_view(&app, wid).unwrap();
    assert_eq!(view.feedback, None, "cleared about five seconds on");
    assert_eq!(view.status_clock(), None);
    assert_eq!(
        app.messages_publish_due_at(),
        Some(cleared + TICK),
        "then only the minute tick"
    );
}

#[test]
fn a_transient_status_line_clears_on_every_page() {
    use crate::app_native::{MESSAGES_FEEDBACK_WAIT as WAIT, MESSAGES_PUBLISH_MIN_GAP as GAP};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().window_on_screen_for_test = true;
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::About));
    let opened = app
        .messages_last_publish
        .expect("opening Settings publishes");
    assert_eq!(
        app.messages_publish_due_at(),
        None,
        "About with no transient line arms nothing"
    );
    fn view_mut(app: &mut App, wid: WindowId) -> &mut crate::native_settings::SettingsViewState {
        let (_, view) = app.active_native_view(wid).expect("Settings fronts it");
        match app.native_runtime.view_state_mut(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => state,
            _ => unreachable!(),
        }
    }
    view_mut(&mut app, wid).show_transient_status_for_test("Build information copied");
    assert_eq!(app.messages_publish_due_at(), Some(opened + GAP));
    app.publish_native_messages_state_if_due(opened + GAP);
    let stamped = app.messages_last_publish.unwrap();
    assert_eq!(settings_view(&app, wid).unwrap().status_clock(), Some(true));
    assert_eq!(app.messages_publish_due_at(), Some(stamped + WAIT));
    view_mut(&mut app, wid)
        .age_transient_status_for_test(crate::native_settings::MESSAGES_FEEDBACK_MS);
    app.publish_native_messages_state_if_due(stamped + WAIT);
    let view = settings_view(&app, wid).unwrap();
    assert_eq!(
        view.feedback, None,
        "cleared about five seconds on, on About"
    );
    assert_eq!(app.messages_publish_due_at(), None, "then nothing is armed");
}

#[test]
fn publish_is_rate_limited_and_only_while_settings_exists() {
    use crate::app_native::MESSAGES_PUBLISH_MIN_GAP as GAP;
    use crate::app_native::MESSAGES_PUBLISH_TICK as TICK;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().window_on_screen_for_test = true;
    let settings_exists = |app: &App| {
        app.native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_some()
    };
    app.post_message(warn("before Settings"));
    app.publish_native_messages_state();
    app.publish_native_messages_state_if_due(std::time::Instant::now() + GAP);
    assert!(!settings_exists(&app));
    assert_eq!(app.messages_last_publish, None, "no Settings, no publish");
    assert_eq!(app.messages_published_revision, 0);
    assert_eq!(app.messages_publish_due_at(), None);

    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    assert!(settings_exists(&app));
    let opened = app
        .messages_last_publish
        .expect("opening Settings publishes");
    let published = app.messages_published_revision;
    assert_eq!(published, app.messages.revision());

    let ids: Vec<MessageId> = (0..4)
        .map(|i| app.post_message(warn(&format!("burst {i}"))))
        .collect();
    assert!(app.resolve_message(ids[0], Outcome::Ok));
    assert_eq!(
        app.messages_last_publish,
        Some(opened),
        "five moves inside 500 ms: not one republish"
    );
    assert_eq!(app.messages_published_revision, published);
    assert!(app.messages.revision() > published);
    assert_eq!(app.messages_publish_due_at(), Some(opened + GAP));
    assert!(
        app.messages_deadline().is_some_and(|d| d <= opened + GAP),
        "the loop is asked back for the pending publish"
    );
    app.publish_native_messages_state_if_due(opened + GAP);
    let second = app.messages_last_publish.expect("published at the gap");
    assert_ne!(second, opened);
    assert_eq!(
        app.messages_published_revision,
        app.messages.revision(),
        "one publish carries the whole burst"
    );
    assert_eq!(
        app.messages_publish_due_at(),
        Some(second + TICK),
        "nothing of the center's pending: only the minute tick, the view on the route"
    );

    assert!(app.close_settings_tabs());
    assert!(!settings_exists(&app), "no view, no controller");
    app.post_message(warn("after close"));
    assert_eq!(
        app.messages_publish_due_at(),
        None,
        "nothing is pending without a page to publish into"
    );
    app.publish_native_messages_state();
    app.publish_native_messages_state_if_due(second + GAP + GAP);
    assert_eq!(app.messages_last_publish, Some(second), "no publish");
    assert_ne!(app.messages.revision(), app.messages_published_revision);

    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    assert_ne!(app.messages_last_publish, Some(second));
    assert_eq!(app.messages_published_revision, app.messages.revision());
}

#[test]
fn a_headless_messages_page_is_republished_only_when_its_time_words_move() {
    type Post = fn(&mut App, Message) -> MessageId;
    let posts: [(&str, Post); 2] = [
        ("post_message", |app, msg| {
            app.post_message(msg.hold(Hold::LogOnly))
        }),
        ("record_message", App::record_message),
    ];
    for zone_s in PINNED_ZONES_S {
        for (at, now, words_move_in) in words_cases(zone_s) {
            for (by, post) in posts {
                let at = format!("{at} in a zone {zone_s} s east, the new record by {by}");
                a_headless_messages_page_republished_at(&at, now, zone_s, words_move_in, post);
            }
        }
    }
}

fn a_headless_messages_page_republished_at(
    at: &str,
    now: u64,
    zone_s: i64,
    words_move_in: std::time::Duration,
    post: fn(&mut App, Message) -> MessageId,
) {
    use crate::app_native::{MESSAGES_PUBLISH_MIN_GAP as GAP, MESSAGES_PUBLISH_TICK as TICK};
    let mut app = App::headless_for_test();
    app.messages_wall = pinned_wall(now, zone_s);
    assert!(app.headless, "the headless lane");
    let wid = WindowId(0);
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    assert!(
        app.messages_state().entries.is_empty(),
        "a fresh instance's log"
    );
    let opened = app
        .messages_last_publish
        .expect("opening Settings publishes");
    assert_eq!(
        app.messages_publish_due_at(),
        None,
        "an empty page says no time: no tick"
    );
    assert_eq!(app.messages_deadline(), None);

    app.post_message_at(
        warn("an old record").hold(Hold::LogOnly),
        aterm_messages::WallStamp {
            unix_ms: now - RECORD_AGE_MS,
        },
    );
    app.publish_native_messages_state_if_due(opened + GAP);
    let published = app.messages_last_publish.unwrap();
    assert_ne!(published, opened);
    assert_eq!(
        app.messages_words_change_in,
        Some(words_move_in),
        "at {at}: when the record's words move"
    );
    assert_eq!(
        app.messages_publish_due_at(),
        Some(published + words_move_in.max(TICK)),
        "at {at}: republished then, and never sooner than the tick"
    );

    post(&mut app, warn("a new record"));
    app.publish_native_messages_state_if_due(published + GAP);
    let fresh = app.messages_last_publish.unwrap();
    assert_eq!(
        app.messages_words_change_in,
        Some(words_move_in.min(std::time::Duration::from_millis(MIN_MS))),
        "at {at}: its `just now` moves a minute on, unless the old record's words move first"
    );
    assert_eq!(app.messages_publish_due_at(), Some(fresh + TICK));

    fn view_mut(app: &mut App, wid: WindowId) -> &mut crate::native_settings::SettingsViewState {
        let (_, view) = app.active_native_view(wid).expect("Settings fronts it");
        match app.native_runtime.view_state_mut(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => state,
            _ => unreachable!(),
        }
    }
    view_mut(&mut app, wid).navigate(SettingsRoute::About);
    assert_eq!(app.messages_publish_due_at(), None, "About says no time");
    view_mut(&mut app, wid).navigate(SettingsRoute::Messages);
    assert_eq!(app.messages_publish_due_at(), Some(fresh + TICK));

    let settings_tab = app.windows[&wid].tab_set.active().unwrap().id;
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .switch_to_index(0)
    );
    assert_ne!(
        app.windows[&wid].tab_set.active().unwrap().id,
        settings_tab,
        "the terminal fronts it"
    );
    assert_eq!(app.messages_publish_due_at(), None, "a background tab");
    assert_eq!(app.messages_deadline(), None);
}

#[test]
fn a_messages_page_that_slept_through_its_words_is_owed_its_publish_on_waking() {
    for zone_s in PINNED_ZONES_S {
        for (at, now, words_move_in) in words_cases(zone_s) {
            let at = format!("{at} in a zone {zone_s} s east");
            a_messages_page_woken_at(&at, now, zone_s, words_move_in);
        }
    }
}

fn a_messages_page_woken_at(at: &str, now: u64, zone_s: i64, words_move_in: std::time::Duration) {
    use crate::app_native::MESSAGES_PUBLISH_TICK as TICK;
    const NIGHT_MS: u64 = 8 * 60 * MIN_MS;
    const NIGHT: std::time::Duration = std::time::Duration::from_millis(NIGHT_MS);
    let mut app = App::headless_for_test();
    app.messages_wall = pinned_wall(now, zone_s);
    let wid = WindowId(0);
    app.post_message_at(
        warn("an old record").hold(Hold::LogOnly),
        aterm_messages::WallStamp {
            unix_ms: now - RECORD_AGE_MS,
        },
    );
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    let published = app
        .messages_last_publish
        .expect("opening Settings publishes");
    assert_eq!(
        app.messages_words_change_in,
        Some(words_move_in),
        "at {at}: when the record's words move"
    );
    let due = published + words_move_in.max(TICK);
    assert_eq!(app.messages_publish_due_at(), Some(due), "at {at}");

    let instant = std::time::Instant::now();
    let wall = std::time::UNIX_EPOCH + std::time::Duration::from_millis(now);
    app.count_sleep_since_last_turn(instant, wall);
    app.count_sleep_since_last_turn(instant, wall - NIGHT);
    assert_eq!(
        app.messages_publish_due_at(),
        Some(due),
        "CONTROL at {at}: a wall clock stepped back moves nothing"
    );
    app.count_sleep_since_last_turn(instant, wall + NIGHT);
    app.messages_wall = pinned_wall(now + NIGHT_MS, zone_s);
    assert_eq!(
        app.messages_publish_due_at(),
        Some(published + TICK),
        "at {at}: owed at once, a minute after the last publish at the soonest"
    );
    app.publish_native_messages_state_if_due(published + TICK);
    let republished = app.messages_last_publish.unwrap();
    assert_ne!(republished, published, "the slept page is republished");
    assert_eq!(
        app.messages_words_change_in,
        Some(std::time::Duration::from_secs(50 * 60)),
        "at {at} a night on, the record's spoken `11 hours ago` moves at its next hour"
    );

    let read_on = app.messages_words_offset_s;
    assert_eq!(
        read_on, zone_s,
        "at {at}: the page says its times in the clock's zone"
    );
    app.messages_words_offset_s = read_on + 3600;
    assert_eq!(app.messages_publish_due_at(), Some(republished + TICK));
    app.publish_native_messages_state_if_due(republished + TICK);
    assert_eq!(
        app.messages_words_offset_s, read_on,
        "the publish reads the zone"
    );
    let last = app.messages_last_publish.unwrap();
    assert_eq!(
        app.messages_publish_due_at(),
        app.messages_words_change_in
            .map(|until| last + until.max(TICK))
    );
}

#[test]
fn a_developer_tools_press_never_answers_the_file_access_card() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let press = Intent::OpenSystemPane {
        pane: PANE_DEVELOPER_TOOLS.into(),
    };
    let row = app.post_message(
        Message::new(
            tags::PRIVACY,
            Severity::Info,
            "aterm is not in Developer Tools",
        )
        .action(press.clone()),
    );
    assert!(
        !app.perform_intent(wid, row, press),
        "a headless arm opens no pane"
    );
    assert_eq!(app.consent_card_host.marker, None, "no file-access marker");
    assert_eq!(app.consent_card_host.record.opened_at, None);
    let open_fda = Intent::OpenSystemPane {
        pane: PANE_FULL_DISK_ACCESS.into(),
    };
    let fda = app.post_message(
        Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
            .action(open_fda.clone()),
    );
    assert!(!app.perform_intent(wid, fda, open_fda));
    assert_eq!(
        app.consent_card_host.marker,
        Some(crate::consent_card::Marker::Opened)
    );
    assert!(app.consent_card_host.record.opened_at.is_some());
}

#[cfg(target_os = "macos")]
#[test]
fn the_developer_tools_press_is_the_one_hand_over() {
    use crate::devtools_helper::test_arms;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = std::time::Instant::now();
    assert!(app.devtools_watch.wants_read(now), "the launch read");
    let press = Intent::OpenSystemPane {
        pane: PANE_DEVELOPER_TOOLS.into(),
    };
    let row = app.post_message(
        Message::new(
            tags::PRIVACY,
            Severity::Info,
            "aterm is not in Developer Tools",
        )
        .action(press.clone()),
    );
    test_arms::stand_in(&mut app, test_arms::refusing());
    assert!(!app.perform_intent(wid, row, press.clone()));
    assert_eq!((test_arms::opens(), test_arms::opened_notes()), (1, 0));
    assert!(app.devtools_helper.panel().is_none(), "no page, no panel");
    assert!(
        !app.devtools_watch.wants_read(now),
        "nothing to come back from"
    );
    test_arms::stand_in(&mut app, test_arms::opening());
    assert!(app.perform_intent(wid, row, press));
    assert_eq!((test_arms::opens(), test_arms::opened_notes()), (2, 1));
    assert!(
        app.devtools_helper
            .panel()
            .is_some_and(|p| p.origin() == crate::devtools_helper::Origin::Digest),
        "the panel beside the page"
    );
    assert!(
        app.devtools_watch.wants_read(now),
        "the owner's return reads"
    );
    assert_eq!(app.consent_card_host.marker, None, "no file-access marker");
    assert_eq!(app.consent_card_host.record.opened_at, None);
}

#[test]
fn open_path_external_refuses_paths_outside_the_log_dir_and_symlinks() {
    assert!(!open_path_external("relative/crash.log"));
    assert!(!open_path_external("/definitely/not/a/file"));
    let root = std::env::temp_dir().join(format!("aterm-open-path-{}", std::process::id()));
    let logs = root.join("logs");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let artifact = logs.join("crash-signal-1-1.log.seen");
    std::fs::write(&artifact, "fatal signal 11").unwrap();
    let outside = elsewhere.join("crash-signal-1-1.log.seen");
    std::fs::write(&outside, "x").unwrap();
    let admits = |p: &Path| admissible_log_path(&p.to_string_lossy(), &logs);
    assert_eq!(
        admits(&artifact),
        Some(artifact.clone()),
        "the artifact the crash reporter minted"
    );
    assert_eq!(
        admits(&outside),
        None,
        "a regular file OUTSIDE the log dir is refused"
    );
    let escape = logs
        .join("..")
        .join("elsewhere")
        .join("crash-signal-1-1.log.seen");
    assert!(
        escape.starts_with(&logs) && std::fs::metadata(&escape).is_ok_and(|m| m.is_file()),
        "the escape starts under the dir lexically and resolves to a regular file\u{2026}"
    );
    assert_eq!(admits(&escape), None, "\u{2026}and is refused for its `..`");
    assert_eq!(admits(&logs), None, "the dir itself");
    assert_eq!(admits(&logs.join("missing.log")), None, "a missing file");
    assert_eq!(
        admissible_log_path("crash-signal-1-1.log.seen", &logs),
        None,
        "a relative path"
    );
    #[cfg(unix)]
    {
        let link = logs.join("crash-signal-2-2.log.seen");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(
            std::fs::metadata(&link).is_ok_and(|m| m.is_file()),
            "the link resolves to a regular file\u{2026}"
        );
        assert_eq!(
            admits(&link),
            None,
            "\u{2026}and is refused for being a link"
        );
        let inner = logs.join("crash-signal-3-3.log.seen");
        std::os::unix::fs::symlink(&artifact, &inner).unwrap();
        assert_eq!(
            admits(&inner),
            None,
            "a link to a file INSIDE the dir is still a link"
        );
    }
    assert!(
        !open_path_external(&outside.to_string_lossy()),
        "through the real log dir: a regular file outside it is refused"
    );
    let _ = std::fs::remove_dir_all(&root);
    let mut app = App::headless_for_test();
    app.perform_intent(
        WindowId(0),
        MessageId::from_raw(1).unwrap(),
        Intent::OpenPath {
            path: "/definitely/not/a/file".into(),
        },
    );
    assert!(
        app.messages
            .live_rows()
            .any(|l| l.msg.detail == ["/definitely/not/a/file"]),
        "the refusal is a row"
    );
}

fn net_read(running: bool, started: u64) -> crate::PkgProgressSnapshot {
    let mut programs = std::collections::BTreeMap::new();
    programs.insert(
        "trust".to_string(),
        atpkg::progress::ProgramProgress {
            phase: if running {
                atpkg::progress::Phase::Extract
            } else {
                atpkg::progress::Phase::Done
            },
            bytes_done: 120_000_000,
            bytes_total: 900_000_000,
            build: Some(5520),
            bumped: false,
            error: None,
        },
    );
    crate::PkgProgressSnapshot {
        file: atpkg::progress::ProgressFile {
            v: atpkg::progress::PROGRESS_VERSION,
            pid: running.then_some(7),
            pass: "net".to_string(),
            started_unix: started,
            generation: None,
            heartbeat_unix: started,
            overall: atpkg::progress::Overall {
                programs_done: 3,
                programs_total: 10,
                bytes_done: 512_000_000,
                bytes_total: 1_200_000_000,
            },
            queue: vec!["ty".to_string()],
            programs,
            ended_unix: (!running).then_some(started + 60),
        },
        running,
    }
}

fn appstatus(app: &App) -> Vec<String> {
    aterm_messages::wire::activity_rows_compat(
        &app.messages,
        Instant::now(),
        &crate::control::pct_encode,
    )
}

#[test]
fn a_routine_pass_raises_no_row_and_records_every_outcome() {
    let mut app = App::headless_for_test();
    app.announce_toolchain_pass(
        "installing 1 program(s) over the network: ty (about 40 MB)",
        false,
    );
    app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_000)), false);
    app.apply_toolchain_snapshot(Some(&light_read(false, 1_700_000_000)), false);
    app.record_toolchain_outcome(toolchain_words::installed(
        "\u{2713} ALab toolchain installed: ty",
    ));
    app.record_toolchain_outcome(toolchain_words::ended(
        "the pass finished; 12 ALab program(s) are installed",
        false,
    ));
    app.post_managed_current("claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)");
    app.post_machine_settings("universal-control disabled");
    app.record_message(toolchain_words::deferred(
        "an earlier toolchain pass is still running",
    ));
    app.toolchain_pass_ended(true);
    assert_eq!(app.messages.live_rows().count(), 0, "no row");
    assert_eq!(app.messages_deadline(), None, "nothing to wake for");
    let rows = appstatus(&app);
    assert_eq!(rows.len(), 5, "every outcome on record: {rows:#?}");
    for row in &rows {
        assert!(
            row.starts_with("activity kind=toolchain phase=done progress=- title=")
                && row.contains(" outcome=ok since_ms="),
            "{row}"
        );
    }
}

#[test]
fn a_first_run_row_holds_across_the_vendor_sub_pass_and_folds_at_exit() {
    for vendor_failed in [false, true] {
        let mut app = App::headless_for_test();
        let mut shown = Vec::new();
        let note = |app: &App, shown: &mut Vec<usize>| {
            shown.push(app.messages.live_rows().count());
        };
        app.announce_toolchain_pass(
            "installing 2 program(s) over the network: claude, codex (about 300 MB)",
            true,
        );
        note(&app, &mut shown);
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        note(&app, &mut shown);
        app.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), true);
        note(&app, &mut shown);
        app.record_toolchain_outcome(if vendor_failed {
            toolchain_words::failed("network provisioning installed nothing")
        } else {
            toolchain_words::installed("claude, codex")
        });
        note(&app, &mut shown);
        app.announce_toolchain_pass(
            "installing 10 ALab program(s) over the network (about 3 GB on disk)",
            true,
        );
        note(&app, &mut shown);
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_100)), true);
        note(&app, &mut shown);
        app.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_100)), true);
        note(&app, &mut shown);
        app.record_toolchain_outcome(toolchain_words::installed("ay, trust"));
        note(&app, &mut shown);
        app.toolchain_pass_ended(true);
        note(&app, &mut shown);
        let changes = shown.windows(2).filter(|w| w[0] != w[1]).count();
        assert_eq!(
            (shown.first(), shown.last(), changes),
            (Some(&1), Some(&0), 1),
            "vendor failed {vendor_failed}: one open, one fold: {shown:?}"
        );
        assert_eq!(appstatus(&app).len(), 2, "both answers are on record");
    }
}

#[test]
fn a_first_run_row_no_update_takes_over_is_withdrawn() {
    let mut app = App::headless_for_test();
    app.open_first_run_ahead();
    let row = app
        .messages
        .live_by_key(toolchain_words::KEY_PASS)
        .map(|l| l.id)
        .expect("the row");
    app.toolchain_pass_ended(true);
    app.withdraw_first_run_ahead();
    assert!(app.messages.live(row).is_none());
    assert_eq!(
        app.messages.log().get(row).unwrap().state,
        LogState::Retired(Retired::Withdrawn)
    );
    assert!(appstatus(&app).is_empty(), "{:?}", appstatus(&app));
    app.toolchain_pass_ended(true);
    assert_eq!(app.messages.live_rows().count(), 0);

    let mut app = App::headless_for_test();
    app.open_first_run_ahead();
    app.toolchain_pass_ended(true);
    app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
    app.withdraw_first_run_ahead();
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "the announcing child's row"
    );
}

#[test]
fn an_answer_folds_a_live_row_no_child_is_latched_behind() {
    let mut app = App::headless_for_test();
    app.post_message(toolchain_words::announced("installing 2 program(s)"));
    app.record_toolchain_outcome(toolchain_words::ended(
        "the pass finished; 12 ALab program(s) are installed",
        false,
    ));
    assert_eq!(app.messages.live_rows().count(), 0, "answered: folded");
    let rows = appstatus(&app);
    assert_eq!(rows.len(), 1, "the answer is on record: {rows:?}");
    assert!(rows[0].contains(" phase=done "), "{}", rows[0]);
}

#[cfg(unix)]
/// A parked outgoing handoff (the parent's half of the freeze predicate) with nothing live behind it, like the handoff tests' `parked_record`.
pub(crate) fn parked() -> crate::PendingUpdateHandoff {
    let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
    crate::PendingUpdateHandoff {
        park_at: Instant::now(),
        proof_ready_at: None,
        attempt_id: 1,
        nonce: None,
        live: Vec::new(),
        adoption: Vec::new(),
        child_pid: None,
        mode: crate::native_updater_service::ApplyMode::Automatic,
        apply_attempt: None,
        same_image: Some(crate::app_update_handoff::SameImageHandoff::DebugSeam),
        target_build: 0,
        target_commit: String::new(),
        layout: crate::restore::RestoreManifest::new(Vec::new()),
        carry_lone_lists: false,
        layout_digest: [0; 32],
        screen_digest: [0; 32],
        activity_epoch: 0,
        hold_serials: 0,
        carried_claims: Vec::new(),
        cancel,
        #[cfg(unix)]
        arbiter: crate::HandoffAttemptArbiter::new(),
        teardown: crate::DeferredHandoffTeardown::None,
        commit_drain_started: None,
        commit_flush: None,
        held_commit: None,
        revoked_by_activity: false,
    }
}

/// The manifest's window carry around what `carried_messages` handed over, as `App::handoff_window_carry` writes it, with no frame, position or font zoom.
pub(super) fn window_carry(
    carried: crate::session_store::MessagesCarry,
) -> crate::session_store::WindowCarry {
    crate::session_store::WindowCarry {
        rows: 24,
        cols: 80,
        outer_x: None,
        outer_y: None,
        inner_px: None,
        presence_row: false,
        messages: carried.messages,
        next_message_id: carried.next_message_id,
        update_verified_unix_ms: None,
        font_px_milli: None,
        font_reset_px_milli: None,
        font_size_milli: None,
        font_reset_size_milli: None,
        update_health_said: carried.update_health_said,
        messages_captured_unix_ms: carried.captured_unix_ms,
    }
}

/// Obsolete join alerts cannot reach the first frame after restoring messages, even
/// before the membership poll has run. Other carried warnings remain visible.
#[test]
fn restoring_messages_retires_obsolete_join_alerts_before_polling() {
    let mut parent = App::headless_for_test();
    for key in ["members.ask.1", "secrets.asking.ABCD", "unrelated.warning"] {
        parent.post_message(warn("Carried warning").key(key));
    }
    let carry = window_carry(parent.carried_messages());
    for finishing in [None, Some("0.149.0")] {
        let mut successor = App::headless_for_test();
        successor.seed_carried_messages(&carry, finishing);
        assert!(successor.messages.live_by_key("members.ask.1").is_none());
        assert!(
            successor
                .messages
                .live_by_key("secrets.asking.ABCD")
                .is_none()
        );
        assert!(
            successor
                .messages
                .live_by_key("unrelated.warning")
                .is_some()
        );
    }
}

#[cfg(unix)]
#[test]
fn the_parents_record_is_its_own_to_the_end_and_the_successors_waits_for_commit() {
    let dir = std::env::temp_dir().join(format!("aterm-messages-freeze-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(crate::messages_store::FILE_NAME);
    let mut app = App::headless_for_test();
    app.messages_log = crate::messages_store::Writer::spawn(&path);
    assert!(app.messages_log.is_some(), "the writer opened");
    let before = app.post_message(warn("before the park"));
    assert_eq!(
        app.messages.log().pending_len(),
        0,
        "an unfrozen post drains to the writer at once"
    );
    let carried = app.carried_messages();
    assert_eq!(carried.messages.len(), 1);
    app.pending_update_handoff = Some(parked());
    assert!(
        app.message_holds_frozen(),
        "the parent's half of the holds predicate"
    );
    assert!(
        !app.message_persist_frozen(),
        "…and the parent's drain never waits"
    );
    let during = app.post_message(warn("during the park"));
    assert_eq!(
        app.messages.log().pending_len(),
        0,
        "the line reached the writer under the park"
    );
    let past_hold = Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(1);
    assert!(
        !app.settle_messages(past_hold),
        "a hold that fell due under the park folds nothing"
    );
    assert!(app.messages.live(before).is_some() && app.messages.live(during).is_some());
    app.flush_messages_log();
    assert!(app.messages_log.is_some());
    let text = std::fs::read_to_string(&path).expect("the log was written");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains("before the park"), "{}", lines[0]);
    assert!(lines[1].contains("during the park"), "{}", lines[1]);
    drop(app.messages_log.take());

    let successor_path = dir.join("successor.log");
    let mut successor = App::headless_for_test();
    successor.messages_log = crate::messages_store::Writer::spawn(&successor_path);
    successor.incoming_handoff_pending = true;
    assert!(successor.message_persist_frozen());
    successor.post_message(warn("before Commit"));
    assert_eq!(
        successor.messages.log().pending_len(),
        1,
        "a successor that may yet be refused writes nothing"
    );
    successor.incoming_handoff_pending = false;
    successor.sync_messages();
    assert_eq!(successor.messages.log().pending_len(), 0, "Commit drains");
    drop(successor.messages_log.take());
    let text = std::fs::read_to_string(&successor_path).expect("written after Commit");
    assert_eq!(text.lines().count(), 1, "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_handoff_carries_live_rows() {
    let mut parent = App::headless_for_test();
    let installing = parent.post_message(update_words::installing("0.76.0"));
    parent.post_message(toolchain_words::announced("installing ay, trust"));
    parent.pending_update_handoff = Some(parked());
    let carry = window_carry(parent.carried_messages());

    let mut successor = App::headless_for_test();
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(&carry, Some("0.76.0"));
    assert_eq!(
        successor
            .messages
            .live_by_key(update_words::KEY_PROGRESS)
            .map(|l| l.id),
        Some(installing),
        "the progress row crossed"
    );
    assert_eq!(
        successor.live_update_flow().map(|f| f.phase),
        Some(FlowPhase::Finishing),
        "the successor adopts the flow row as its own"
    );
    assert_eq!(
        successor.messages.log().pending_len(),
        0,
        "nothing crossed as a log line"
    );

    let mut plain = App::headless_for_test();
    plain.seed_carried_messages(&carry, None);
    assert_eq!(
        plain
            .messages
            .live_by_key(update_words::KEY_PROGRESS)
            .unwrap()
            .msg
            .detail,
        vec![update_words::INSTALLING],
        "its words as they were"
    );

    let mut other = App::headless_for_test();
    other.post_message(toolchain_words::announced("installing ay"));
    let no_progress = window_carry(other.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&no_progress, Some("0.76.0"));
    assert!(
        successor
            .messages
            .live_by_key(update_words::KEY_PROGRESS)
            .is_none()
    );

    let old: crate::session_store::WindowCarry = aterm_toml::from_str(
        "rows = 24\ncols = 80\nstatus_bar_rows = 1\n\n[[bars]]\nlane = \"toolchain\"\n\
         glyph = \"!\"\ntitle = \"ALab toolchain\"\ndetail = \"\"\ntone = \"warn\"\n\
         fill_permille = 430\n",
    )
    .expect("an older parent's window carry still decodes, its band rows read past");
    assert_eq!((old.rows, old.cols), (24, 80));
    let mut adopting = App::headless_for_test();
    adopting.seed_carried_messages(&old, Some("0.76.0"));
    assert_eq!(
        adopting.messages.live_rows().count(),
        0,
        "no bar is carried"
    );
    let mut empty = App::headless_for_test();
    empty.seed_carried_messages(&window_carry(Default::default()), Some("0.76.0"));
    assert_eq!(empty.messages.live_rows().count(), 0);
    assert_eq!(empty.messages.log().next_id(), MessageId::FIRST);
}

/// A v0.147.0 parent, the last whose band took rows, carried `status_bar_rows = 3` and rows
/// `on_glass`. Both keys are read past: the successor's window takes the exact frame of the
/// carried grid (the band's rows are dropped, the grid keeps its size), and nothing re-grids from
/// the seed to Commit.
#[cfg(unix)]
#[test]
fn a_v0_147_parents_band_is_read_past_and_nothing_regrids_to_commit() {
    let desk = aterm_tempfile::manifest_dir().join("tests/fixtures/handoff/v0.147.0/carried-state");
    let manifest = std::fs::read_dir(&desk)
        .expect("the shipped desk")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with("seamless-")
                    && name.ends_with(".toml")
                    && !name.ends_with(".layout.toml")
            })
        })
        .expect("the desk's manifest");
    let text = std::fs::read_to_string(&manifest).expect("the manifest reads");
    let (_nonce, toml) = text.split_once('\n').expect("the nonce line");
    let band: u16 = 3;
    assert!(
        toml.contains(&format!("status_bar_rows = {band}")) && toml.contains("on_glass = true"),
        "PRECONDITION: the parent committed band rows and carried rows on its glass"
    );
    let handoff: crate::session_store::SessionHandoff =
        aterm_toml::from_str(toml).expect("a v0.147.0 manifest still decodes");
    let carry = handoff.window.expect("the window carry");
    let [width, height] = carry.inner_px.expect("the carried frame");
    assert!(
        width.is_multiple_of(u32::from(carry.cols))
            && height.is_multiple_of(u32::from(carry.rows + band)),
        "PRECONDITION: the carried frame is the grid and the band's rows, in whole cells"
    );

    let wid = crate::WindowId(0);
    let mut successor = App::headless_for_test();
    let exact = successor.window_frame_px_for(wid, carry.rows, carry.cols);
    // The desk's frame is in the cells of the face that generated it (1407x462 is 7x14 px
    // cells); this host's face may lay others (Linux's headless face lays 9x20), so the parent
    // here carried the same grid and band rows in this face's cells.
    let carried = successor.window_frame_px_for(wid, carry.rows + band, carry.cols);
    assert!(
        carried.height > exact.height,
        "PRECONDITION: the carried frame holds the band's rows"
    );
    let _ = crate::app_presence::take_chrome_regrids_applied();
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(&carry, Some("0.148.0"));
    assert!(
        successor.messages.live_rows().count() > 0,
        "PRECONDITION: rows crossed"
    );
    let frame = successor.attach_frame_px(wid, carry.rows, carry.cols, Some(carried));
    assert_eq!(frame, exact, "the exact frame of the carried grid");
    successor.on_resize(wid, frame);
    let grid = (carry.rows, carry.cols);
    let still = |app: &App, at: &str| {
        assert_eq!(
            app.windows.get(&wid).map(|ws| (ws.rows, ws.cols)),
            Some(grid),
            "{at}: the carried grid"
        );
    };
    still(&successor, "seeded");
    successor.sync_messages();
    still(&successor, "a frozen sync");
    successor.incoming_handoff_pending = false;
    successor.messages.after_handoff_commit(Instant::now());
    successor.release_successor_commit_freeze();
    still(&successor, "Commit");
    assert_eq!(
        crate::app_presence::take_chrome_regrids_applied(),
        Vec::new(),
        "no chrome re-grid from the seed to Commit"
    );
}

/// A current parent carries no band rows and no glass: its rows cross, and the successor's
/// Commit re-grids no window's chrome.
#[cfg(unix)]
#[test]
fn a_current_parents_rows_cross_and_commit_regrids_nothing() {
    let mut parent = App::headless_for_test();
    parent.post_message(update_words::installing("0.76.0"));
    parent.post_message(toolchain_words::announced("installing ay, trust"));
    parent.post_message(warn("a carried warning"));
    parent.pending_update_handoff = Some(parked());
    let carry = window_carry(parent.carried_messages());
    assert!(
        carry.messages.len() >= 2,
        "PRECONDITION: rows to carry: {:?}",
        carry.messages
    );

    let mut successor = App::headless_for_test();
    let _ = crate::app_presence::take_chrome_regrids_applied();
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(&carry, Some("0.76.0"));
    successor.sync_messages();
    successor.incoming_handoff_pending = false;
    successor.messages.after_handoff_commit(Instant::now());
    successor.release_successor_commit_freeze();
    assert!(
        successor.messages.live_rows().count() >= 2,
        "the rows cross Commit"
    );
    assert_eq!(
        crate::app_presence::take_chrome_regrids_applied(),
        Vec::new(),
        "Commit re-grids no window's chrome"
    );
}

#[cfg(unix)]
#[test]
fn a_record_the_seed_moves_off_a_carried_id_lands_above_the_parked_band() {
    let mut parent = App::headless_for_test();
    let row = parent.post_message(warn("Parent row"));
    let carried = parent.carried_messages();
    assert!(carried.messages.iter().any(|m| m.id == row.raw()));
    let mut boot = aterm_messages::MessageLog::empty();
    boot.replay_all([
        aterm_messages::LogLine::Posted(LogRecord::from_posted(
            row,
            aterm_messages::WallStamp { unix_ms: 1 },
            &Message::new(tags::RENDER, Severity::Info, "Other writer"),
        )),
        aterm_messages::LogLine::Retired {
            id: row,
            how: Retired::Folded,
            unix_ms: 2,
            title: "Other writer".into(),
            detail: Vec::new(),
            repeats: 1,
            mark: None,
        },
    ]);
    let mut successor = App::headless_for_test();
    successor.messages = crate::messages_host::new_center(boot, Instant::now());
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(&window_carry(carried.clone()), Some("0.76.0"));
    assert_eq!(
        successor.messages.live(row).map(|l| l.msg.title.as_str()),
        Some("Parent row"),
        "the carried id is the live row's"
    );
    let squatter = successor
        .messages
        .log()
        .records()
        .find(|r| r.title == "Other writer")
        .expect("the other writer's record stays");
    let band = carried.next_message_id..carried.next_message_id + PARKED_PARENT_ID_RESERVE;
    assert!(
        !band.contains(&squatter.id.raw()) && squatter.id.raw() >= band.end,
        "the moved record took {} inside the parked parent's band {band:?}",
        squatter.id
    );
}

#[cfg(unix)]
#[test]
fn a_landed_update_leaves_no_outcome_row_that_said_it_had_not() {
    for outcome in [
        update_words::failed(
            crate::app_update_screen::UPDATE_WAITS_FOR_YOU,
            App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            true,
        ),
        update_words::needs_install(
            "Couldn't install aterm v9.9.9",
            update_words::INSTALL_FROM_MENU,
            Severity::Warn,
            7,
        ),
    ] {
        let title = outcome.title.clone();
        let mut parent = App::headless_for_test();
        parent.note_update_outcome(outcome);
        parent.sync_messages();
        assert!(
            parent
                .messages
                .live_by_key(update_words::KEY_OUTCOME)
                .is_some(),
            "{title}: the outcome is up"
        );
        parent.begin_update_installing(false);
        let carry = window_carry(parent.carried_messages());
        let mut successor = App::headless_for_test();
        successor.seed_carried_messages(&carry, Some("9.9.9"));
        successor.messages.after_handoff_commit(Instant::now());
        successor.post_update_landed("9.9.9", 7, 0);
        assert!(
            successor
                .messages
                .live_by_key(update_words::KEY_OUTCOME)
                .is_none(),
            "{title}: a landed update leaves no row saying it did not land"
        );
        assert!(
            successor.messages.log().records().any(|r| r.title == title),
            "{title}: the record keeps what it said"
        );
    }
}

#[cfg(unix)]
fn window_seen(app: &mut App) {
    for ws in app.windows.values_mut() {
        ws.window_on_screen_for_test = true;
        ws.occluded = false;
    }
}

#[cfg(unix)]
fn landed_successor(automatic: bool, raise: impl FnOnce(&mut App)) -> App {
    let mut parent = App::headless_for_test();
    raise(&mut parent);
    if !automatic {
        parent.post_message(update_words::installing("9.9.9"));
    }
    let carry = window_carry(parent.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.messages.after_handoff_commit(Instant::now());
    successor.post_update_landed("9.9.9", 7, 0);
    successor
}

#[cfg(unix)]
fn healings(app: &App) -> Vec<String> {
    app.messages
        .log()
        .records()
        .filter(|r| r.title == "aterm updates work again")
        .map(|r| r.detail.join(" "))
        .collect()
}

/// Whether `app` owes the digest line of `kind`: open, or claimed to ripen.
#[cfg(unix)]
pub(super) fn owes_entry(app: &App, kind: update_words::HealthKind) -> bool {
    app.owes_exception(&update_words::update_health_entry(kind, "").key())
}

#[cfg(unix)]
fn successor_beat(checks: u64, deferrals: u64) -> aterm_update::checker_watch::CheckerBeat {
    aterm_update::checker_watch::CheckerBeat {
        generation: 1,
        phase: aterm_update::checker_watch::CheckerPhase::Waiting,
        at_secs: 100,
        max_interval_secs: 1_100,
        respawns: 0,
        workers: 1,
        worker_epoch: 0,
        deferrals,
        settings_misses: 0,
        lane: None,
        checks,
    }
}

#[cfg(unix)]
#[test]
fn a_carried_unhealed_stall_never_silences_the_successors_own_watchdog() {
    use crate::update_checker_watch::CheckerStall;
    use aterm_update::checker_watch::{CheckerBeat, CheckerPhase};
    let stalled = update_words::HealthKind::Stalled;
    for carried in [true, false] {
        let mut parent = App::headless_for_test();
        window_seen(&mut parent);
        if carried {
            parent.announce_update_checker_stall(
                CheckerStall {
                    generation: 1,
                    for_secs: 3_000,
                    phase: CheckerPhase::LockWait,
                    holds_lane: true,
                    checks: 4,
                },
                Some(2),
            );
            parent.settle_messages(
                Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(60),
            );
            assert!(parent.live_update_health(|_| true).is_empty(), "folded");
        }
        let carry = window_carry(parent.carried_messages());
        assert_eq!(carry.update_health_said.len(), usize::from(carried));
        let mut successor = App::headless_for_test();
        successor.seed_carried_messages(&carry, Some("9.9.9"));
        successor.messages.after_handoff_commit(Instant::now());
        successor.post_update_landed("9.9.9", 7, 0);
        successor.announce_update_checker_stall(
            CheckerStall {
                generation: 1,
                for_secs: 3_000,
                phase: CheckerPhase::Checking,
                holds_lane: false,
                checks: 0,
            },
            Some(2),
        );
        assert_eq!(
            successor.live_update_health(|k| k == stalled).len(),
            1,
            "the successor's own stall is a live row (carried: {carried})"
        );
        let beat = CheckerBeat {
            generation: 2,
            phase: CheckerPhase::Waiting,
            at_secs: 100,
            max_interval_secs: 1_100,
            respawns: 1,
            workers: 2,
            worker_epoch: 0,
            deferrals: 0,
            settings_misses: 0,
            lane: None,
            checks: 1,
        };
        successor.look_at_update_checker(Some(beat), 110);
        assert!(
            !successor.update_health_latched.contains(&stalled),
            "a completed check heals the watchdog's warning (carried: {carried})"
        );
        assert!(successor.live_update_health(|k| k == stalled).is_empty());
        assert_eq!(
            healings(&successor).len(),
            1,
            "one healing, against the successor's own words (carried: {carried})"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_carried_check_warning_heals_by_the_successors_first_healthy_check() {
    let check = update_words::HealthKind::Check;
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    let title = aterm_update::health_failing_title("manifest");
    for automatic in [false, true] {
        UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().clear());
        let mut successor = landed_successor(automatic, |parent| {
            assert!(parent.note_update_health(title, body));
        });
        assert_eq!(
            UPDATER_ADOPTED_HEALTH.with(|adopted| std::mem::take(&mut *adopted.borrow_mut())),
            vec!["manifest"],
            "the parent's class is this process's updater's to heal (automatic: {automatic})"
        );
        assert!(
            !successor.update_health_latched.contains(&check),
            "never latched here (automatic: {automatic})"
        );
        assert!(
            owes_entry(&successor, check),
            "its digest line crosses the update (automatic: {automatic})"
        );
        assert!(!successor.note_update_health(aterm_update::HEALTH_RESTATED_TITLE, body));
        assert!(healings(&successor).is_empty());
        assert_eq!(
            successor.live_update_health(|k| k == check).len(),
            1,
            "the carried check warning stands, alone (automatic: {automatic})"
        );
        assert!(!successor.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, "manifest"));
        assert_eq!(
            healings(&successor),
            vec!["after \"Couldn't check for updates\", since Sep 14".to_string()],
            "the healing is on record against what the parent said (automatic: {automatic})"
        );
        assert!(successor.live_update_health(|k| k == check).is_empty());
        assert!(!owes_entry(&successor, check), "and its digest line");
    }
    UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().clear());
    let _folded = landed_successor(false, |parent| {
        window_seen(parent);
        assert!(parent.note_update_health(title, body));
        parent.settle_messages(
            Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(60),
        );
        assert!(parent.live_update_health(|_| true).is_empty(), "folded");
    });
    assert_eq!(
        UPDATER_ADOPTED_HEALTH.with(|adopted| std::mem::take(&mut *adopted.borrow_mut())),
        vec!["manifest"]
    );
    UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().clear());
    let _quiet = landed_successor(true, |_| {});
    assert!(UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow().is_empty()));
}

#[cfg(unix)]
#[test]
fn a_carried_warnings_digest_line_says_what_the_parents_would_have() {
    let check = update_words::HealthKind::Check;
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    let title = aterm_update::health_failing_title("manifest");
    let key = update_words::update_health_entry(check, "").key();
    let words = |app: &App| -> Vec<String> {
        let digest = app.messages.digest();
        let entry = digest.entries.iter().find(|e| e.key == key);
        entry.map(|e| e.detail.clone()).unwrap_or_default()
    };
    let mut parent = App::headless_for_test();
    assert!(parent.note_update_health(title, body));
    let in_process = words(&parent);
    assert!(
        in_process.iter().any(|l| l == "failing since Sep 14")
            && in_process.iter().any(|l| l.contains("the reason")),
        "{in_process:?}"
    );
    for automatic in [false, true] {
        let successor = landed_successor(automatic, |parent| {
            assert!(parent.note_update_health(title, body));
        });
        assert_eq!(words(&successor), in_process, "automatic: {automatic}");
    }
    let wire = aterm_json::to_string(&parent.carried_health_said()).expect("the wire");
    let mut cold = App::headless_for_test();
    cold.take_up_carried_health_said(&crate::app_update_handoff::parse_cold_lane_health_said(
        Some(std::ffi::OsStr::new(&wire)),
    ));
    assert_eq!(words(&cold), in_process, "the cold lane");
    let mut older = parent.carried_health_said();
    older.iter_mut().for_each(|said| said.body.clear());
    let mut cold = App::headless_for_test();
    cold.take_up_carried_health_said(&older);
    assert!(owes_entry(&cold, check));
    assert_ne!(words(&cold), in_process);
}

#[cfg(unix)]
#[test]
fn a_carried_lock_held_warning_is_the_successors_watchdog_to_heal() {
    use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
    let held = update_words::HealthKind::LockHeld;
    let stalled = update_words::HealthKind::Stalled;
    let stall_body = update_words::checker_stalled_body(3_000, "checking", true, false);
    let rows = |app: &App| app.live_update_health(|k| k == held).len();
    for (kind, automatic) in [
        (stalled, false),
        (stalled, true),
        (held, false),
        (held, true),
    ] {
        let case = format!("{kind:?}, automatic: {automatic}");
        let mut successor = landed_successor(automatic, |parent| {
            if kind == held {
                parent.announce_checker_lock_held(CHECKER_UNGATED_AFTER);
            } else {
                assert!(parent.note_update_health_as(
                    stalled,
                    update_words::CHECKER_STALLED_TITLE,
                    &stall_body
                ));
            }
        });
        assert!(!successor.update_health_latched.contains(&kind), "{case}");
        successor.look_at_update_checker(None, 100);
        successor.look_at_update_checker(Some(successor_beat(0, 0)), 110);
        assert!(
            healings(&successor).is_empty(),
            "NEGATIVE: no check has completed here yet ({case})"
        );
        assert!(owes_entry(&successor, kind), "its digest line crosses");
        successor.look_at_update_checker(Some(successor_beat(1, 0)), 200);
        assert!(successor.live_update_health(|k| k == kind).is_empty());
        assert!(
            !owes_entry(&successor, kind),
            "its digest line heals with it ({case})"
        );
        assert_eq!(
            healings(&successor).len(),
            1,
            "a check completed without deferring heals it, on record ({case})"
        );
        if kind == stalled {
            continue;
        }
        successor.look_at_update_checker(Some(successor_beat(2, CHECKER_UNGATED_AFTER)), 300);
        assert_eq!(
            rows(&successor),
            1,
            "a new streak speaks (automatic: {automatic})"
        );

        let mut successor = landed_successor(automatic, |parent| {
            parent.announce_checker_lock_held(CHECKER_UNGATED_AFTER);
        });
        let carried_rows = rows(&successor);
        successor.look_at_update_checker(Some(successor_beat(0, CHECKER_UNGATED_AFTER)), 110);
        successor.look_at_update_checker(Some(successor_beat(2, CHECKER_UNGATED_AFTER + 2)), 700);
        assert_eq!(
            rows(&successor),
            carried_rows,
            "NEGATIVE: the same sibling is not said a second time"
        );
        assert!(
            healings(&successor).is_empty(),
            "checks without the lock heal nothing"
        );
        successor.look_at_update_checker(Some(successor_beat(3, 0)), 1_300);
        assert_eq!(rows(&successor), 0);
        assert_eq!(healings(&successor).len(), 1);
    }
}

#[cfg(unix)]
#[test]
fn a_carried_strand_notice_is_not_the_updaters_to_heal() {
    let check = update_words::HealthKind::Check;
    let body = "aterm cannot read its release channel: no release carries a manifest.";
    let adopted = || UPDATER_ADOPTED_HEALTH.with(|a| std::mem::take(&mut *a.borrow_mut()));
    for automatic in [false, true] {
        let _ = adopted();
        let mut successor = landed_successor(automatic, |parent| {
            assert!(parent.note_update_health(aterm_update::STRANDED_TITLE, body));
        });
        assert_eq!(
            adopted(),
            Vec::<&str>::new(),
            "the strand is no class the updater announced (automatic: {automatic})"
        );
        assert!(!successor.update_health_latched.contains(&check));
        assert!(owes_entry(&successor, check), "its digest line crosses");
        assert!(successor.note_update_health(aterm_update::STRANDED_TITLE, body));
        assert_eq!(
            successor.live_update_health(|k| k == check).len(),
            1,
            "one row: the carried one leaves for its successor (automatic: {automatic})"
        );
        assert!(
            healings(&successor).is_empty(),
            "no healing on record while the machine is stranded (automatic: {automatic})"
        );
    }
    for (title, handed) in [
        ("aterm can't check for updates", vec!["manifest"]),
        ("Couldn't download updates", vec!["pipeline"]),
        ("Couldn't reach the future", Vec::new()),
    ] {
        let _ = adopted();
        let _successor = landed_successor(true, |parent| {
            assert!(parent.note_update_health(title, body));
        });
        assert_eq!(adopted(), handed, "{title:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_cold_lane_landing_records_the_healing_of_the_parents_install_warning() {
    let body = "3 failed attempts to install in a row since 2026-09-24T01:00:00Z: why.";
    let mut parent = App::headless_for_test();
    assert!(parent.note_update_health(aterm_update::health_failing_title("apply"), body));
    let mut command = std::process::Command::new("/usr/bin/true");
    crate::app_update_handoff::bind_cold_lane_health_said(
        &mut command,
        &parent.carried_health_said(),
    );
    let wire = command
        .get_envs()
        .find(|(key, _)| *key == crate::app_update_handoff::COLD_LANE_HEALTH_SAID_ENV)
        .and_then(|(_, value)| value)
        .map(std::ffi::OsStr::to_os_string);
    assert!(wire.is_some(), "the cold arm names it on its exec");
    for carried in [true, false] {
        let said = crate::app_update_handoff::parse_cold_lane_health_said(
            wire.as_deref().filter(|_| carried),
        );
        let mut successor = App::headless_for_test();
        successor.take_up_carried_health_said(&said);
        successor.post_update_landed("9.9.9", 7, 0);
        assert_eq!(
            healings(&successor),
            if carried {
                vec!["after \"Couldn't install updates\", since Sep 24".to_string()]
            } else {
                Vec::new()
            },
            "carried: {carried}"
        );
    }
    assert!(
        crate::app_update_handoff::parse_cold_lane_health_said(Some(std::ffi::OsStr::new(
            "not json"
        )))
        .is_empty()
    );
}

#[cfg(unix)]
const COMMIT_FLUSH_IN_TESTS: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

#[cfg(unix)]
#[test]
fn a_late_park_successor_sees_the_parents_records_written_after_its_boot() {
    let dir = std::env::temp_dir().join(format!("aterm-messages-late-park-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(crate::messages_store::FILE_NAME);
    let mut parent = App::headless_for_test();
    parent.messages_log = crate::messages_store::Writer::spawn(&path);
    let x = parent.post_message(warn("open at the boot"));
    parent.flush_messages_log();
    let mut successor = App::headless_for_test();
    successor.messages = crate::messages_host::new_center(
        crate::messages_store::load_tail(&path, u64::MAX).log,
        Instant::now(),
    );
    assert_eq!(
        successor.messages.log().get(x).and_then(LogRecord::retired),
        Some(&Retired::Stale),
        "the boot reads the parent's open row as nobody's"
    );
    successor.incoming_handoff_pending = true;
    parent.resolve_message(x, Outcome::Ok);
    let r = parent.record_message(warn("written after the boot"));
    let carry = window_carry(parent.carried_messages());
    parent.pending_update_handoff = Some(parked());
    assert!(
        parent.flush_messages_log_within(COMMIT_FLUSH_IN_TESTS),
        "the writer landed everything"
    );
    drop(parent);
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.incoming_handoff_pending = false;
    successor.messages.after_handoff_commit(Instant::now());
    successor.messages_log = crate::messages_store::Writer::spawn(&path);
    successor.reload_messages_log_at_commit();
    let log = successor.messages.log();
    assert!(
        matches!(
            log.get(x).and_then(LogRecord::retired),
            Some(Retired::Resolved(_))
        ),
        "the parent's ending, not Stale: {:?}",
        log.get(x).map(|rec| &rec.state)
    );
    assert!(
        log.records()
            .any(|rec| rec.title == "written after the boot"),
        "the record the parent wrote after the boot is in the history"
    );
    let next = successor.post_message(warn("the successor's own"));
    assert!(next > r, "ids continue past the parent's: {next} after {r}");
    drop(successor.messages_log.take());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_row_the_parked_parent_ended_after_the_capture_stays_ended() {
    let ids = std::cell::Cell::new(None);
    let (mut successor, dir) = committed_after_park(
        "parked-endings",
        |parent| {
            say_check_warning(parent);
            let resolved =
                parent.post_message(warn("a reporter ends this one").key("test.resolved"));
            let open = parent.post_message(warn("nobody ends this one").key("test.open"));
            ids.set(Some((resolved, open)));
        },
        |parent| {
            parent.heal_update_health(HealthProof::Checked);
            let (resolved, _) = ids.get().expect("posted");
            parent.resolve_message(resolved, Outcome::Ok);
        },
    );
    let (resolved, open) = ids.get().expect("posted");
    assert!(
        successor.live_update_health(|_| true).is_empty(),
        "the warning the parent healed stays healed"
    );
    assert!(
        successor.messages.live(resolved).is_none(),
        "the row its reporter ended stays ended"
    );
    assert!(
        successor.messages.live(open).is_some(),
        "a carried row nobody ended is the successor's now"
    );
    successor.heal_update_health(HealthProof::Checked);
    assert_eq!(healing_count(&successor), 1, "one healing, the parent's");
    drop(successor.messages_log.take());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_health_warning_the_parked_parent_dismissed_is_still_healed_on_record() {
    let (mut successor, dir) =
        committed_after_park("parked-dismissal", say_check_warning, |parent| {
            let warning = parent.live_update_health(|_| true);
            assert_eq!(warning.len(), 1, "PRECONDITION: the warning is up");
            assert!(parent.messages.dismiss(warning[0], Instant::now()));
        });
    assert!(
        successor.live_update_health(|_| true).is_empty(),
        "the row the person dismissed stays dismissed"
    );
    assert!(
        successor
            .update_health_said
            .iter()
            .any(|said| said.kind == update_words::HealthKind::Check),
        "a dismissal records no healing: what the warning said stays for the proof"
    );
    assert!(
        owes_entry(&successor, update_words::HealthKind::Check),
        "and the day's entry of a failure nobody healed stays claimed"
    );
    successor.heal_update_health(HealthProof::Checked);
    assert_eq!(
        healing_count(&successor),
        1,
        "its healing is recorded, once, by the proof here"
    );
    drop(successor.messages_log.take());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
fn committed_after_park(
    name: &str,
    raise: impl FnOnce(&mut App),
    while_parked: impl FnOnce(&mut App),
) -> (App, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("aterm-messages-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(crate::messages_store::FILE_NAME);
    let mut parent = App::headless_for_test();
    parent.messages_log = crate::messages_store::Writer::spawn(&path);
    raise(&mut parent);
    parent.sync_messages();
    let carry = window_carry(parent.carried_messages());
    parent.pending_update_handoff = Some(parked());
    while_parked(&mut parent);
    assert!(parent.flush_messages_log_within(COMMIT_FLUSH_IN_TESTS));
    drop(parent);
    let mut successor = App::headless_for_test();
    successor.messages_log = crate::messages_store::Writer::spawn(&path);
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.incoming_handoff_pending = false;
    successor.reload_messages_log_at_commit();
    successor.messages.after_handoff_commit(Instant::now());
    (successor, dir)
}

#[cfg(unix)]
fn say_check_warning(app: &mut App) {
    assert!(app.note_update_health_as(
        update_words::HealthKind::Check,
        "Couldn't check for updates",
        "3 failed checks in a row since 2026-09-28T00:00:00Z: x",
    ));
}

#[cfg(unix)]
fn healing_count(app: &App) -> usize {
    app.messages
        .log()
        .records()
        .filter(|rec| rec.title == "aterm updates work again")
        .count()
}

#[cfg(unix)]
#[test]
fn a_healing_the_parked_parent_recorded_is_on_record_once_whatever_its_row_did() {
    for (name, before_park, in_park) in [
        (
            "parked-dismiss-heal",
            (|_: &mut App| {}) as fn(&mut App),
            (|parent: &mut App| {
                let warning =
                    parent.live_update_health(|kind| kind == update_words::HealthKind::Check);
                assert_eq!(warning.len(), 1, "PRECONDITION: the warning is up");
                assert!(parent.messages.dismiss(warning[0], Instant::now()));
            }) as fn(&mut App),
        ),
        (
            "parked-folded-heal",
            (|parent: &mut App| {
                parent.settle_messages(
                    Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(60),
                );
                assert!(
                    parent.live_update_health(|_| true).is_empty(),
                    "PRECONDITION: the warnings folded before the park"
                );
            }) as fn(&mut App),
            (|_: &mut App| {}) as fn(&mut App),
        ),
    ] {
        let (mut successor, dir) = committed_after_park(
            name,
            |parent| {
                window_seen(parent);
                say_check_warning(parent);
                assert!(parent.note_update_health_as(
                    update_words::HealthKind::Download,
                    aterm_update::health_failing_title("pipeline"),
                    "4 failed downloads in a row since 2026-09-28T00:00:00Z: y",
                ));
                before_park(parent);
            },
            |parent| {
                in_park(parent);
                parent.heal_update_health(HealthProof::Checked);
            },
        );
        assert_eq!(
            healing_count(&successor),
            1,
            "{name}: the parent's healing is in the history"
        );
        assert!(
            !successor
                .update_health_said
                .iter()
                .any(|said| said.kind == update_words::HealthKind::Check),
            "{name}: what the healed warning said left with its healing"
        );
        assert!(
            !owes_entry(&successor, update_words::HealthKind::Check),
            "{name}: the healed failure's day's entry is not owed here"
        );
        assert!(
            successor
                .live_update_health(|kind| kind == update_words::HealthKind::Check)
                .is_empty(),
            "{name}: no row of the healed warning stands after Commit"
        );
        successor.heal_update_health(HealthProof::Checked);
        assert_eq!(
            healing_count(&successor),
            1,
            "{name}: the successor's own proof records no second healing"
        );
        assert!(
            successor
                .update_health_said
                .iter()
                .any(|said| said.kind == update_words::HealthKind::Download),
            "{name}: the download warning the parent did not heal stays for the proof here"
        );
        successor.heal_update_health(HealthProof::Downloaded);
        assert_eq!(
            healing_count(&successor),
            2,
            "{name}: the download warning's healing is recorded here, once"
        );
        drop(successor.messages_log.take());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(unix)]
#[test]
fn an_earlier_healing_in_the_same_words_does_not_answer_a_carried_warning() {
    let (mut successor, dir) = committed_after_park(
        "parked-earlier-healing",
        |parent| {
            say_check_warning(parent);
            parent.heal_update_health(HealthProof::Checked);
            say_check_warning(parent);
        },
        |_| {},
    );
    assert_eq!(
        healing_count(&successor),
        1,
        "PRECONDITION: the first episode's healing is in the history"
    );
    assert!(
        successor
            .update_health_said
            .iter()
            .any(|said| said.kind == update_words::HealthKind::Check),
        "the second episode is still this process's to heal"
    );
    successor.heal_update_health(HealthProof::Checked);
    assert_eq!(
        healing_count(&successor),
        2,
        "each episode's healing is on record once"
    );
    drop(successor.messages_log.take());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_kind_the_parked_parent_healed_leaves_no_row_whatever_the_file_says_of_it() {
    let mut parent = App::headless_for_test();
    say_check_warning(&mut parent);
    parent.sync_messages();
    let carry = window_carry(parent.carried_messages());
    let from = carry.next_message_id;
    let mut healer = App::headless_for_test();
    healer.messages.raise_next_id(from);
    say_check_warning(&mut healer);
    healer.heal_update_health(HealthProof::Checked);
    let disk = healer.messages.log().clone();

    let mut successor = App::headless_for_test();
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.incoming_handoff_pending = false;
    let parked = successor
        .parked_parent
        .take()
        .expect("the carry marks where the parent's records begin");
    assert_eq!(
        successor
            .live_update_health(|kind| kind == update_words::HealthKind::Check)
            .len(),
        1,
        "PRECONDITION: the warning's row is carried"
    );
    successor.forget_health_the_parked_parent_healed(&disk, parked);
    assert!(
        !successor
            .update_health_said
            .iter()
            .any(|said| said.kind == update_words::HealthKind::Check),
        "the parent's healing is read"
    );
    assert!(
        successor
            .live_update_health(|kind| kind == update_words::HealthKind::Check)
            .is_empty(),
        "and the healed warning's row ends with its memory"
    );
    assert_eq!(healing_count(&successor), 0, "with no record of its own");
}

#[cfg(unix)]
#[test]
fn a_healed_warning_s_row_ends_though_another_writer_took_its_number() {
    let name = "parked-collided-heal";
    let path = std::env::temp_dir()
        .join(format!("aterm-messages-{name}-{}", std::process::id()))
        .join(crate::messages_store::FILE_NAME);
    let (successor, dir) = committed_after_park(
        name,
        |parent| {
            let mut w2 = App::headless_for_test();
            w2.messages_log = crate::messages_store::Writer::spawn(&path);
            let next = parent.messages.log().next_id().raw();
            w2.messages.raise_next_id(next);
            w2.record_message(warn("another window's note").hold(Hold::LogOnly));
            assert!(w2.flush_messages_log_within(COMMIT_FLUSH_IN_TESTS));
            drop(w2);
            say_check_warning(parent);
            let row = parent.live_update_health(|kind| kind == update_words::HealthKind::Check);
            assert_eq!(
                row.first().map(|id| id.raw()),
                Some(next),
                "PRECONDITION: the warning's row takes the other window's number"
            );
        },
        |parent| parent.heal_update_health(HealthProof::Checked),
    );
    assert!(
        successor
            .live_update_health(|kind| kind == update_words::HealthKind::Check)
            .is_empty(),
        "the row the parent's heal resolved stays ended"
    );
    assert_eq!(healing_count(&successor), 1, "healed on record once");
    let mut successor = successor;
    drop(successor.messages_log.take());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn another_writers_healing_in_the_same_words_is_not_the_parked_parents() {
    for (name, w2_above_band, in_park) in [
        ("w2-above-band-before", true, false),
        ("w2-above-band-in-park", true, true),
        ("w2-in-band-before", false, false),
    ] {
        let path = std::env::temp_dir()
            .join(format!("aterm-messages-{name}-{}", std::process::id()))
            .join(crate::messages_store::FILE_NAME);
        let w2_heals = |parent: &App| {
            let mut w2 = App::headless_for_test();
            w2.messages_log = crate::messages_store::Writer::spawn(&path);
            let next = parent.messages.log().next_id().raw();
            w2.messages.raise_next_id(if w2_above_band {
                next + 10 * PARKED_PARENT_ID_RESERVE
            } else {
                next.saturating_sub(2).max(1)
            });
            say_check_warning(&mut w2);
            w2.heal_update_health(HealthProof::Checked);
            assert!(w2.flush_messages_log_within(COMMIT_FLUSH_IN_TESTS));
            if !w2_above_band {
                let healing = w2
                    .messages
                    .log()
                    .records()
                    .rev()
                    .find(|rec| rec.title == "aterm updates work again")
                    .expect("W2's healing");
                assert_eq!(
                    healing.id.raw(),
                    next,
                    "PRECONDITION: W2's healing takes the parent's next number"
                );
            }
            drop(w2);
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let (mut successor, dir) = committed_after_park(
            name,
            |parent| {
                say_check_warning(parent);
                if !in_park {
                    w2_heals(parent);
                }
            },
            |parent| {
                if in_park {
                    w2_heals(parent);
                }
            },
        );
        assert_eq!(
            healing_count(&successor),
            1,
            "{name}: PRECONDITION: W2's healing is in the history"
        );
        assert!(
            successor
                .update_health_said
                .iter()
                .any(|said| said.kind == update_words::HealthKind::Check),
            "{name}: the parent's warning, still failing, is still this process's"
        );
        assert!(
            owes_entry(&successor, update_words::HealthKind::Check),
            "{name}: and its day's entry is still owed"
        );
        successor.heal_update_health(HealthProof::Checked);
        assert_eq!(
            healing_count(&successor),
            2,
            "{name}: its own healing is recorded here, by the proof that answers it"
        );
        drop(successor.messages_log.take());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(unix)]
#[test]
fn a_record_the_parked_parent_makes_never_shares_an_id_with_the_successor() {
    let dir = std::env::temp_dir().join(format!("aterm-messages-park-ids-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(crate::messages_store::FILE_NAME);
    let mut parent = App::headless_for_test();
    parent.messages_log = crate::messages_store::Writer::spawn(&path);
    parent.record_message(warn("before the boot").hold(Hold::LogOnly));
    parent.flush_messages_log();
    let mut successor = App::headless_for_test();
    successor.messages = crate::messages_host::new_center(
        crate::messages_store::load_tail(&path, u64::MAX).log,
        Instant::now(),
    );
    successor.incoming_handoff_pending = true;
    let carry = window_carry(parent.carried_messages());
    parent.pending_update_handoff = Some(parked());
    assert_eq!(
        parent.take_app_notice("toolchain", "aterm pkg: a note while parked"),
        Ok(crate::AppNoticeTaken::Recorded)
    );
    assert!(parent.record_harness_note("a harness note while parked"));
    let parents_last = parent.messages.log().next_id().raw() - 1;
    assert!(
        parents_last >= carry.next_message_id,
        "the parent minted past the carry's next id"
    );
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    let own = successor.record_message(warn("the successor's own").hold(Hold::LogOnly));
    assert!(parent.flush_messages_log_within(COMMIT_FLUSH_IN_TESTS));
    drop(parent);
    successor.incoming_handoff_pending = false;
    successor.messages_log = crate::messages_store::Writer::spawn(&path);
    successor.reload_messages_log_at_commit();
    successor.post_update_landed("9.9.9", 7, 0);
    let landed = successor
        .messages
        .log()
        .records()
        .find(|r| r.title.contains("9.9.9") && r.id > own)
        .map(|r| r.id)
        .expect("the landing's record");
    assert!(
        own.raw() > parents_last,
        "{own} after the parent's {parents_last}"
    );
    let rows = successor.read_messages(&wire::ReadQuery {
        since: MessageId::from_raw(parents_last),
        ..wire::ReadQuery::default()
    });
    for id in [own, landed] {
        assert!(
            rows.iter()
                .any(|row| row.starts_with(&format!("message {id} "))),
            "a reader past the parent's last id sees {id}: {rows:?}"
        );
    }
    for title in ["a note while parked", "a harness note while parked"] {
        assert!(
            successor
                .messages
                .log()
                .records()
                .any(|r| r.title.contains(title) && r.id.raw() <= parents_last),
            "{title}: the parent's record is in the history under its own id"
        );
    }
    successor.flush_messages_log();
    let file = std::fs::read_to_string(&path).expect("messages.log");
    let mut posted: Vec<u64> = file
        .lines()
        .filter_map(|line| match aterm_messages::LogLine::decode(line) {
            Ok(aterm_messages::LogLine::Posted(rec)) => Some(rec.id.raw()),
            _ => None,
        })
        .collect();
    let n = posted.len();
    posted.sort_unstable();
    posted.dedup();
    assert_eq!(posted.len(), n, "messages.log names each id once: {file}");
    drop(successor.messages_log.take());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_notice_while_parked_is_refused_for_a_retry_and_mints_nothing() {
    let mut parent = App::headless_for_test();
    let carry = window_carry(parent.carried_messages());
    parent.pending_update_handoff = Some(parked());
    let before = parent.messages.log().next_id();
    let req = || wire::NoticeRequest::parse("post script sev=warn Deploy failed").expect("a post");
    let reply = parent.take_notice(req());
    assert_eq!(reply, wire::switching_reply());
    assert!(reply.starts_with("ERR busy notice: "), "{reply}");
    assert!(reply.ends_with("retry_ms=1000"), "{reply}");
    assert_eq!(parent.messages.log().next_id(), before, "no id handed out");
    assert!(
        !parent
            .messages
            .live_rows()
            .any(|l| l.msg.title == "Deploy failed")
    );
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    let next = successor.post_message(warn("after commit"));
    assert!(next >= before);
    parent.pending_update_handoff = None;
    let reply = parent.take_notice(req());
    assert!(reply.starts_with("OK message="), "{reply}");
}

#[cfg(unix)]
#[test]
fn a_carried_dead_publisher_row_leaves_with_the_process_it_described() {
    let mut parent = App::headless_for_test();
    parent.post_message(crate::message_reporters::a11y_publisher_dead(
        "bus gone", true,
    ));
    parent.post_message(warn("a config warning"));
    let _ = parent
        .settle_messages(Instant::now() + PROGRESS_GRACE + std::time::Duration::from_millis(10));
    let carried = parent.carried_messages();
    assert_eq!(carried.messages.len(), 2, "both rows are carried");
    let carry = window_carry(carried);
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.messages.after_handoff_commit(Instant::now());
    assert!(
        successor
            .messages
            .live_by_key(crate::message_reporters::KEY_A11Y_PUBLISHER)
            .is_none(),
        "the old process's publisher is not this one's"
    );
    assert!(
        successor
            .messages
            .live_rows()
            .any(|l| l.msg.title == "a config warning"),
        "a row about something else still crosses"
    );
}

#[cfg(unix)]
#[test]
fn a_carried_recovery_row_offers_no_end_sessions_that_ends_nothing() {
    let mut parent = App::headless_for_test();
    let id = parent.post_message(crate::message_reporters::keeper_recovered_message(
        2,
        0,
        crate::crash_restore::RungCounts::default(),
        0,
        None,
        None,
    ));
    let _ = parent
        .settle_messages(Instant::now() + PROGRESS_GRACE + std::time::Duration::from_millis(10));
    let carry = window_carry(parent.carried_messages());
    assert!(
        carry.messages.iter().any(|m| m.id == id.raw()),
        "premise: the recovery row crosses"
    );
    let offers_end = |app: &App| {
        app.messages
            .live(id)
            .map(|l| l.msg.actions.contains(&Intent::EndRecovered))
    };
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.messages.after_handoff_commit(Instant::now());
    assert!(
        successor.keeper_recovered.is_empty(),
        "premise: the successor reattached nothing itself"
    );
    assert_eq!(
        offers_end(&successor),
        Some(false),
        "the carried row stands without `End sessions`"
    );
    let mut reattached = App::headless_for_test();
    reattached.keeper_recovered = vec![4242];
    reattached.seed_carried_messages(&carry, Some("9.9.9"));
    assert_eq!(
        offers_end(&reattached),
        Some(true),
        "a process that reattached shells still offers to end them"
    );
}

fn light_read(running: bool, started: u64) -> crate::PkgProgressSnapshot {
    let mut snap = net_read(running, started);
    snap.file.overall.bytes_done = 12_000_000;
    snap.file.overall.bytes_total = 40_000_000;
    snap
}

#[test]
fn an_automatic_download_that_fails_is_a_record_with_its_version() {
    let mut app = App::headless_for_test();
    app.note_update_progress(&aterm_update::Progress::Downloading {
        version: "9.9.9".into(),
        bytes_done: 10_000_000,
        bytes_total: 0,
    });
    app.note_update_progress(&aterm_update::Progress::Failed {
        detail: "connection reset".into(),
    });
    assert_eq!(app.messages.live_rows().count(), 0);
    assert!(app.update_quiet.is_none());
    assert!(
        app.messages
            .log()
            .records()
            .any(|r| r.severity == Severity::Warn && r.title.contains("9.9.9")),
        "the failure's record names the version"
    );
}

#[test]
fn a_navigation_press_folds_a_held_row() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let packages = Intent::OpenSettings {
        route: "/packages".into(),
    };
    let held = app.post_message(warn("Packages need attention").action(packages.clone()));
    assert!(app.perform_intent(wid, held, packages.clone()));
    assert!(app.messages.live(held).is_none(), "the held row folded");
    assert!(!app.messages.log().get(held).is_some_and(LogRecord::is_live));
    let live = app.post_message(
        Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab toolchain")
            .action(packages.clone())
            .hold(Hold::Live {
                stale_after: aterm_messages::STALE_ANNOUNCE,
            }),
    );
    assert!(app.perform_intent(wid, live, packages));
    assert!(app.messages.live(live).is_some(), "work in flight stays");
}

#[test]
fn a_persons_install_and_remove_take_their_rows() {
    use crate::message_reporters::{Attention, PackagesVerb, attention};
    let mut app = App::headless_for_test();
    app.person_pass(PersonPass::Began(PackagesVerb::Install));
    app.announce_toolchain_pass(
        "installing 10 ALab program(s) over the network (about 3 GB on disk when finished)",
        false,
    );
    let live = app.messages.live_rows().next().expect("the Install's row");
    let title = live.msg.title.clone();
    let meter = live.msg.meter.as_ref().expect("busy meter");
    assert!(meter.busy);
    assert!(
        meter.stats.contains("~3 GB"),
        "the announced size: {}",
        meter.stats
    );
    assert_eq!(meter.load, Some(aterm_messages::Load::Network));
    assert_eq!(attention(&live.msg), Ok(Attention::Progress));
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
    let live = app.messages.live_rows().next().expect("restated");
    assert_eq!(
        live.msg.title, title,
        "the plan keeps the Install's own title"
    );
    app.person_pass(PersonPass::Ended(EchoKind::Fault));
    assert_eq!(app.messages.live_rows().count(), 0);

    let mut rm = App::headless_for_test();
    rm.person_pass(PersonPass::Began(PackagesVerb::Remove));
    let live = rm.messages.live_rows().next().expect("the Remove's row");
    assert_eq!(
        live.msg.meter.as_ref().and_then(|m| m.load),
        Some(aterm_messages::Load::Disk)
    );
    assert_eq!(attention(&live.msg), Ok(Attention::Progress));
    rm.person_pass(PersonPass::Ended(EchoKind::Complete));
    assert_eq!(rm.messages.live_rows().count(), 0);
}

fn download(version: &str) -> aterm_update::Progress {
    aterm_update::Progress::Downloading {
        version: version.into(),
        bytes_done: 45_000_000,
        bytes_total: 74_000_000,
    }
}

fn post_ready_staged(app: &mut App, version: &str, build: u64) -> MessageId {
    app.post_update_row(
        update_words::staged(
            version,
            build,
            Some(update_words::ApplyPosture::ManualByConfig),
        ),
        version,
        FlowPhase::Staged { build },
    )
}

#[cfg(unix)]
fn post_installing(app: &mut App, version: &str) -> MessageId {
    app.post_update_row(
        update_words::installing(version),
        version,
        FlowPhase::Installing,
    )
}

fn update_row(app: &App) -> Option<String> {
    app.messages
        .live_by_key(update_words::KEY_PROGRESS)
        .map(|l| format!("{} \u{2014} {}", l.msg.title, l.msg.detail.join("; ")))
}

#[test]
fn a_held_update_row_replaced_by_a_newer_one_is_resolved_on_record() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    app.note_update_check_asked(true);
    app.note_update_progress(&download("9.9.9"));
    app.note_update_progress(&aterm_update::Progress::Failed {
        detail: "connection reset".into(),
    });
    assert_eq!(update_row(&app), None, "a self-retry is a record");
    app.note_update_progress(&download("9.9.9"));
    app.note_update_outcome(update_words::needs_install("first", "", Severity::Warn, 7));
    app.note_update_outcome(update_words::needs_install("second", "", Severity::Warn, 7));
    let done: Vec<String> = appstatus(&app)
        .into_iter()
        .filter(|r| r.contains(" phase=done "))
        .collect();
    assert!(
        done.iter().any(
            |r| r.contains("title=Couldn%27t%20download%20aterm%20v9.9.9")
                || r.contains("title=Couldn't%20download%20aterm%20v9.9.9")
        ),
        "{done:#?}"
    );
    assert!(done.iter().any(|r| r.contains("title=first ")), "{done:#?}");
    assert!(
        done.iter()
            .any(|r| r.contains("title=Downloading%20stopped ")
                && r.contains("detail=Downloading%20aterm%20v9.9.9")),
        "the download the failure ended is on record, as its outcome: {done:#?}"
    );
}

#[test]
fn the_landing_is_on_record_with_how_long_the_update_took() {
    let took = |app: &App| {
        app.messages
            .log()
            .records()
            .flat_map(|r| r.detail.iter())
            .filter(|l| l.contains("after the download"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let now = Instant::now();
    let wall = std::time::SystemTime::now();
    let ms = update_words::verified_unix_ms(
        Some((7, now - std::time::Duration::from_secs(42))),
        7,
        now,
        wall,
    );
    let mut app = App::headless_for_test();
    app.update_verified = update_words::carried_verification(ms, 7, now, wall);
    app.post_update_landed("9.9.9", 7, 0);
    // The landing reads its own clock: the span is the 42 s carried, which the carry's cut to
    // the millisecond can make a second more, plus however long this test took to get there.
    let late = now.elapsed().as_secs();
    assert_eq!(
        update_row(&app),
        None,
        "the landing takes no row (ruling 141)"
    );
    assert_eq!(
        app.messages
            .log()
            .records()
            .filter(|r| r.title.contains("aterm v9.9.9"))
            .count(),
        1,
        "the landing is ONE entry"
    );
    let said = took(&app);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        (42..=43 + late).any(|secs| said[0]
            == format!(
                "installed {} after the download",
                update_words::span_words(std::time::Duration::from_secs(secs))
            )),
        "{said:?}"
    );
    let mut disk = App::headless_for_test();
    disk.post_update_landed("9.9.9", 7, 0);
    assert!(took(&disk).is_empty());
    let mut other = App::headless_for_test();
    other.update_verified = Some((6, Instant::now()));
    other.post_update_landed("9.9.9", 7, 0);
    assert!(took(&other).is_empty());
    let mut none = App::headless_for_test();
    none.update_verified = update_words::carried_verification(None, 7, now, wall);
    none.post_update_landed("9.9.9", 7, 0);
    assert!(took(&none).is_empty());
}

#[test]
fn the_ledgers_heal_answers_only_the_class_it_counted() {
    let apply = aterm_update::health_failing_title("apply");
    let body = "automatic apply of build 9 stopped after 2 failed handoffs";
    let up = |app: &App| app.messages.live_health().is_some();
    let healed = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.title == "aterm updates work again")
            .count()
    };
    for (class, answers) in [
        ("manifest", false),
        ("stage", false),
        ("apply", true),
        ("", true),
    ] {
        let mut app = App::headless_for_test();
        assert!(app.note_update_health(apply, body));
        assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, class));
        assert_eq!(!up(&app), answers, "{class:?}: the row");
        assert_eq!(healed(&app), usize::from(answers), "{class:?}: the record");
    }
}

#[test]
fn a_warning_heals_only_on_the_proof_that_answers_it() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    let healed = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.title == "aterm updates work again")
            .count()
    };
    let warning_up = |app: &App| app.messages.live_health().is_some();
    let mut check = App::headless_for_test();
    assert!(check.note_update_health(aterm_update::health_failing_title("manifest"), body));
    check.note_update_progress(&download("9.9.9"));
    assert!(!warning_up(&check));
    assert_eq!(healed(&check), 1);
}

#[test]
fn the_landing_records_the_healing_with_no_progress_row_and_after_the_fold() {
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    let healed = |successor: &App| {
        successor
            .messages
            .log()
            .records()
            .filter(|r| r.title == "aterm updates work again")
            .map(|r| r.detail.join(" "))
            .collect::<Vec<_>>()
    };
    let through_the_wire = |carry: &crate::session_store::WindowCarry| {
        let text = aterm_toml::to_string(carry).expect("encodes");
        aterm_toml::from_str::<crate::session_store::WindowCarry>(&text).expect("decodes")
    };
    let mut carry = None;
    for automatic in [false, true] {
        for gone in [false, true] {
            let case = format!("automatic {automatic}, gone from the glass {gone}");
            let mut parent = App::headless_for_test();
            if !automatic {
                parent.post_message(update_words::installing("0.76.0"));
            }
            assert!(parent.note_update_health(aterm_update::health_failing_title("apply"), body));
            if gone {
                for id in parent.live_update_health(|_| true) {
                    parent.resolve_message(id, Outcome::Warn);
                }
            }
            let crossed = through_the_wire(&window_carry(parent.carried_messages()));
            assert!(!crossed.update_health_said.is_empty(), "{case}");
            let mut successor = App::headless_for_test();
            successor.seed_carried_messages(&crossed, Some("0.76.0"));
            if !automatic {
                assert!(
                    successor.messages.live_health().is_none(),
                    "{case}: resolved with the handoff"
                );
            }
            successor.post_update_landed("0.76.0", 7, 0);
            assert_eq!(
                healed(&successor),
                vec!["after \"Couldn't install updates\", since Sep 14".to_string()],
                "{case}: once, against what the parent said"
            );
            assert!(successor.messages.live_health().is_none(), "{case}");
            carry = Some(crossed);
        }
    }
    let carry = carry.expect("a carry");
    let mut plain = App::headless_for_test();
    plain.seed_carried_messages(&carry, None);
    plain.post_update_landed("0.76.0", 7, 0);
    assert!(healed(&plain).is_empty());
    let quiet = window_carry(App::headless_for_test().carried_messages());
    let text = aterm_toml::to_string(&quiet).expect("encodes");
    assert!(!text.contains("update_health_said"), "{text}");
}

#[test]
fn the_heal_is_keyed_by_kind_and_an_older_builds_words_still_heal() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    let staged = aterm_update::Progress::Staged {
        version: "9.9.9".into(),
        build: 7,
    };
    let health_rows = |app: &App| {
        app.messages
            .live_rows()
            .filter(|l| update_words::HealthKind::of_row(&l.msg).is_some())
            .map(|l| (l.msg.key.clone().unwrap_or_default(), l.msg.title.clone()))
            .collect::<Vec<_>>()
    };
    let older = |title: &str| {
        let mut msg = update_words::health_warning(update_words::HealthKind::Check, title, body);
        msg.key = Some(update_words::KEY_HEALTH.to_string());
        msg
    };
    let carried = |title: &str| {
        let mut parent = App::headless_for_test();
        parent.post_message(older(title));
        let carry = window_carry(parent.carried_messages());
        let mut app = App::headless_for_test();
        app.seed_carried_messages(&carry, None);
        assert_eq!(health_rows(&app).len(), 1, "{title}: carried");
        app
    };

    let mut app = carried("aterm can't download updates");
    app.note_update_progress(&download("9.9.9"));
    assert_eq!(
        health_rows(&app).len(),
        1,
        "NEGATIVE: the old words are the download half, not the check half"
    );
    app.note_update_progress(&staged);
    assert!(health_rows(&app).is_empty(), "healed by a staged build");

    let mut app = carried("aterm can't install updates");
    app.note_update_progress(&download("9.9.9"));
    app.note_update_progress(&staged);
    assert_eq!(health_rows(&app).len(), 1, "NEGATIVE: not by a download");
    app.post_update_landed("9.9.9", 7, 0);
    assert!(health_rows(&app).is_empty(), "healed by the landing");

    let mut app = carried("aterm's update check stopped");
    app.heal_update_checker_stall();
    assert!(health_rows(&app).is_empty(), "the stall's own heal");

    let mut app = carried("aterm can't download updates");
    assert!(app.note_update_health(aterm_update::health_failing_title("stage"), body));
    assert_eq!(
        health_rows(&app),
        vec![(
            update_words::HealthKind::Download.key().to_string(),
            "Couldn't download updates".to_string()
        )]
    );
    app.note_update_progress(&staged);
    let said: Vec<String> = app
        .messages
        .log()
        .records()
        .filter(|r| r.title == "aterm updates work again")
        .map(|r| r.detail.join(" "))
        .collect();
    assert_eq!(
        said,
        vec!["after \"Couldn't download updates\", since Sep 14".to_string()]
    );

    let mut app = App::headless_for_test();
    app.post_message(update_words::health_warning(
        update_words::HealthKind::Install,
        aterm_update::health_failing_title("pipeline"),
        body,
    ));
    app.note_update_progress(&staged);
    assert_eq!(health_rows(&app).len(), 1, "NEGATIVE: the key says install");
    app.post_update_landed("9.9.9", 7, 0);
    assert!(health_rows(&app).is_empty());
}

#[test]
fn two_health_kinds_live_each_record_their_own_healing() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    let staged = aterm_update::Progress::Staged {
        version: "9.9.9".into(),
        build: 7,
    };
    let healed = |app: &App| -> Vec<String> {
        app.messages
            .log()
            .records()
            .filter(|r| r.title == "aterm updates work again")
            .map(|r| r.detail.join(" "))
            .collect()
    };
    let after = |title: &str| format!("after \"{title}\", since Sep 14");
    let live_kinds = |app: &App| -> Vec<update_words::HealthKind> {
        app.messages
            .live_rows()
            .filter_map(|l| update_words::HealthKind::of_row(&l.msg))
            .collect()
    };
    let download = aterm_update::health_failing_title("stage");
    let install = aterm_update::health_failing_title("apply");

    let mut app = App::headless_for_test();
    assert!(app.note_update_health(download, body));
    assert!(app.note_update_health(install, body));
    assert_eq!(live_kinds(&app).len(), 2, "two halves, two rows");
    app.note_update_progress(&staged);
    assert_eq!(
        healed(&app),
        vec![after(download)],
        "the download's healing is on record — the install warning said \
         after it did not take it"
    );
    assert_eq!(
        live_kinds(&app),
        vec![update_words::HealthKind::Install],
        "NEGATIVE: a staged build is no proof that installing works"
    );
    app.post_update_landed("9.9.9", 7, 0);
    assert_eq!(healed(&app), vec![after(download), after(install)]);
    app.post_update_landed("9.9.9", 7, 0);
    assert_eq!(healed(&app).len(), 2, "NEGATIVE: each healing once");

    let mut app = App::headless_for_test();
    assert!(app.note_update_health(install, body));
    assert!(app.note_update_health(download, body));
    app.post_update_landed("9.9.9", 7, 0);
    assert_eq!(
        live_kinds(&app),
        vec![update_words::HealthKind::Download],
        "NEGATIVE: a landing is no proof that downloading works"
    );
    assert_eq!(healed(&app), vec![after(install)]);
    app.note_update_progress(&staged);
    assert!(live_kinds(&app).is_empty());
    assert_eq!(healed(&app), vec![after(install), after(download)]);

    let mut app = App::headless_for_test();
    assert!(app.note_update_health(install, body));
    assert!(app.note_update_health_as(
        update_words::HealthKind::Stalled,
        update_words::CHECKER_STALLED_TITLE,
        body,
    ));
    app.heal_update_checker_stall();
    assert_eq!(
        healed(&app),
        vec![after(update_words::CHECKER_STALLED_TITLE)]
    );
    assert_eq!(
        live_kinds(&app),
        vec![update_words::HealthKind::Install],
        "NEGATIVE: the stall's heal is not the ledger's"
    );
    app.post_update_landed("9.9.9", 7, 0);
    assert_eq!(healed(&app).len(), 2);
    assert_eq!(healed(&app)[1], after(install));
}

#[test]
fn every_update_moment_is_a_line_in_the_log() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    app.note_update_check_asked(true);
    app.note_update_progress(&download("9.9.9"));
    app.note_update_progress(&aterm_update::Progress::Verifying {
        version: "9.9.9".into(),
    });
    post_ready_staged(&mut app, "9.9.9", 7);
    #[cfg(unix)]
    {
        app.record_update_switch_started(6, false, true);
        app.record_update_switch_stopped(true, "you were typing");
        let before = app.messages.log().len();
        app.record_update_switch_started(6, false, true);
        assert_eq!(app.messages.log().len(), before, "one record per build");
    }
    app.post_update_landed("9.9.9", 7, 0);
    let phases: std::collections::BTreeSet<String> = app
        .messages
        .log()
        .records()
        .filter(|r| r.key.as_deref() == Some(update_words::KEY_PROGRESS))
        .map(|r| r.title.clone())
        .collect();
    assert_eq!(
        phases.len(),
        3,
        "downloading, checking and staged each a line (a title per phase, \
         ruling 143): {phases:#?}"
    );
}

#[cfg(unix)]
#[test]
fn the_switch_record_never_doubles_a_posted_installing_row() {
    let mut quiet = App::headless_for_test();
    let before = quiet.messages.log().len();
    quiet.record_update_switch_started(6, false, true);
    assert_eq!(quiet.messages.log().len(), before + 1, "no row: the record");

    let mut explicit = App::headless_for_test();
    post_installing(&mut explicit, "9.9.9");
    let before = explicit.messages.log().len();
    explicit.record_update_switch_started(6, false, false);
    assert_eq!(
        explicit.messages.log().len(),
        before,
        "the posted Installing row is the line"
    );
    explicit.record_update_switch_stopped(false, "the proof timed out");
    assert_eq!(explicit.messages.log().len(), before + 1);
}

#[test]
fn a_harness_note_is_recorded_and_never_takes_a_row() {
    let mut app = App::headless_for_test();
    assert_eq!(
        app.take_app_notice("harness", "aterm harness acting \u{00b7} approving rm"),
        Ok(crate::AppNoticeTaken::Recorded)
    );
    assert_eq!(app.messages.live_rows().count(), 0);
    assert_eq!(app.messages_deadline(), None);
    let len = app.messages.log().len();
    assert!(!app.record_harness_note("aterm harness acting \u{00b7} approving rm"));
    assert_eq!(
        app.messages.log().len(),
        len,
        "a repeat is not written again"
    );
    assert!(app.record_harness_note("aterm harness acting \u{00b7} approving ls"));
    assert_eq!(app.messages.log().len(), len + 1);
    app.announce_toolchain_pass("installing 2 ALab program(s) (about 1 GB)", true);
    app.record_harness_note("aterm harness idle");
    assert_eq!(app.messages.live_rows().count(), 1);
    assert!(
        app.messages
            .live_by_key(toolchain_words::KEY_PASS)
            .is_some()
    );
}

/// A codex-alab act record as `appnotice harness` carries it: exactly 512 bytes, the act's words
/// first and `tail` (its fact, its why and its turn) whole at its end.
fn act_record(tail: &str) -> String {
    let head = "codex-alab @s-7a87fc5eca87be9d2ffb: typed \"[aterm harness] Autopilot is Cruise: \
                keep going. Finish, verify and land the work, then find the next improvement. ";
    let mut text = head.to_string();
    while text.len() + tail.len() < 512 {
        text.push_str("Decide and record why for later AI review. ");
    }
    text.truncate(512 - tail.len());
    text.push_str(tail);
    text
}

/// THE MESSAGES LOG KEEPS A HARNESS NOTE WHOLE (gap 23): a 512-byte codex-alab act record whose
/// rider sits past the title's cut is on record whole, its title the note cut to `TITLE_CAP`
/// characters with an ellipsis (its first 119 and `…`) and its detail the whole text in lines of
/// at most `DETAIL_LINE_CAP` characters that join back into it. A note the title holds whole gets no detail. NEGATIVE CONTROL: the title
/// alone does not carry the rider.
#[test]
fn a_harness_note_longer_than_its_title_is_recorded_whole() {
    let mut app = App::headless_for_test();
    let rider = " (aterm: a background call (pid 74736) has made no progress for 4 min)";
    let text = act_record(&format!(
        "{rider}\" (continue@v1 at a turn end; turn 019a5f3c-7d21-7c43-9e0b-2f6a1d4c8e90)"
    ));
    assert_eq!(text.len(), 512);
    assert!(
        text.find(rider)
            .is_some_and(|at| at > aterm_messages::TITLE_CAP)
    );
    assert!(app.record_harness_note(&text));
    let rec = app
        .messages
        .log()
        .records()
        .rfind(|r| r.tag.as_str() == "harness")
        .expect("the note's record");
    let kept: String = text.chars().take(aterm_messages::TITLE_CAP - 1).collect();
    assert_eq!(rec.title, format!("{kept}\u{2026}"));
    assert!(!rec.title.contains(rider), "the title alone is cut");
    assert_eq!(rec.detail.concat(), text, "the detail is the whole note");
    assert_eq!(rec.detail.len(), 3);
    assert!(
        rec.detail
            .iter()
            .all(|l| l.chars().count() <= aterm_messages::DETAIL_LINE_CAP)
    );
    assert!(app.record_harness_note("aterm harness idle"));
    let short = app
        .messages
        .log()
        .records()
        .rfind(|r| r.tag.as_str() == "harness")
        .expect("the short note's record");
    assert_eq!(
        (short.title.as_str(), short.detail.len()),
        ("aterm harness idle", 0)
    );
}

/// A REPEAT IS JUDGED BY THE WHOLE NOTE: two act records whose titles match (every Cruise
/// continuation's do) and whose tails differ are two records; a note identical to the last one
/// recorded is not written again, and one identical to an earlier note but not the last is (A,
/// B, A writes A twice). NEGATIVE CONTROL: the two share their title.
#[test]
fn harness_notes_that_differ_past_their_title_are_two_records() {
    let mut app = App::headless_for_test();
    let harness = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.tag.as_str() == "harness")
            .count()
    };
    let plain = act_record("\" (continue@v1 at a turn end; turn 019a5f3c-0001)");
    let ridden = act_record(
        " (aterm: a background call (pid 74736) has made no progress for 4 min)\" \
         (continue@v1 at a turn end; turn 019a5f3c-0002)",
    );
    assert_eq!(
        crate::message_reporters::harness_note(&plain).title,
        crate::message_reporters::harness_note(&ridden).title
    );
    let before = harness(&app);
    assert!(app.record_harness_note(&plain));
    assert!(app.record_harness_note(&ridden));
    assert_eq!(harness(&app), before + 2, "two acts, two records");
    assert!(!app.record_harness_note(&ridden));
    assert_eq!(
        harness(&app),
        before + 2,
        "an identical repeat is not written again"
    );
    assert!(app.record_harness_note(&plain), "A, B, A writes A twice");
    assert_eq!(harness(&app), before + 3);
}

#[test]
fn an_unchanged_managed_current_is_recorded_once_and_news_again() {
    let mut app = App::headless_for_test();
    let managed = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some(toolchain_words::KEY_MANAGED))
            .count()
    };
    let wire = "claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)";
    assert!(app.post_managed_current(wire));
    assert!(!app.post_managed_current(wire), "the same words: not again");
    assert_eq!(managed(&app), 1);
    assert!(
        app.post_managed_current(
            "claude 2.1.281 (Anthropic latest); codex 0.156.0 (OpenAI latest)"
        )
    );
    assert_eq!(managed(&app), 2, "a moved version is news");
    app.seamless_adopt = vec![crate::spawn::Adopted {
        local_id: 3,
        master: crate::spawn::HandedMaster::new(-1),
        pid: -1,
        sid: aterm_session::SessionId::generate(),
        nonce: aterm_session::LaunchNonce::generate(),
        checkpoint: None,
        control: None,
        frozen_path: true,
        topics: Vec::new(),
        rekey: false,
        loader: false,
        repaint: false,
        fg_holder: 0,
        history: crate::handoff_history::AdoptedHistory::default(),
        holds: Vec::new(),
        title: String::new(),
        supervisor: None,
        attention_owners: Vec::new(),
        claim_grace: false,
        outgoing_build: None,
        keeper: None,
        render_debt: None,
        fabric: Default::default(),
        timeline_id: 0,
        log: Default::default(),
        journal_key: None,
        task: None,
        autopilot_end: None,
        wakes: None,
        agent_act: None,
    }];
    assert!(
        app.post_managed_current(
            "claude 2.1.281 (Anthropic latest); codex 0.156.0 (OpenAI latest)"
        ),
        "a frozen tab counted is new words"
    );
    assert_eq!(managed(&app), 2, "said in place: no new record");
    assert!(
        !app.post_managed_current(""),
        "an empty body records nothing"
    );
}

fn notice(app: &mut App, line: &str) -> String {
    let req = wire::NoticeRequest::parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    app.take_notice(req)
}

fn replied_id(reply: &str, field: &str) -> MessageId {
    let raw = reply
        .split_whitespace()
        .find_map(|w| w.strip_prefix(field))
        .unwrap_or_else(|| panic!("{reply:?} has no {field}"));
    MessageId::from_raw(raw.parse().expect("a number")).expect("a nonzero id")
}

#[test]
fn a_wire_progress_flood_keeps_one_row_per_key_and_repaints_nothing() {
    let mut app = App::headless_for_test();
    let asks_before = frame_asks(&app);
    let first = notice(&mut app, "progress build pct=0 Building aterm");
    let id = replied_id(&first, "message=");
    for i in 1..500_u32 {
        let pct = f64::from(i) / 5.0;
        let reply = notice(
            &mut app,
            &format!("progress build pct={pct:.1} Building aterm"),
        );
        assert_eq!(replied_id(&reply, "message="), id, "one row per key");
    }
    let mut n = 0_u32;
    while app.wire_gate.due().is_none() && n < 64 {
        n += 1;
        let _ = notice(
            &mut app,
            &format!("progress build done={n}/100 Building aterm"),
        );
    }
    let due = app.wire_gate.due().expect("a paced paint is armed");
    let deadline = app.messages_deadline();
    let gate = std::mem::take(&mut app.wire_gate);
    assert_eq!(
        app.messages_deadline(),
        deadline,
        "the paced paint is no term of the deadline: it wakes nothing"
    );
    app.wire_gate = gate;
    assert!(
        !app.settle_messages(due),
        "the paced paint asks no repaint: nothing the GUI paints reads the glass"
    );
    // The settle took the paint at `due`, so a restatement a whole gap later paints at once.
    let past = due + aterm_messages::WIRE_PAINT_GAP;
    let hang = Instant::now() + std::time::Duration::from_secs(60);
    while Instant::now() < past {
        assert!(
            Instant::now() < hang,
            "the clock passed no paint gap in a minute"
        );
        std::thread::sleep(past.saturating_duration_since(Instant::now()));
    }
    let asks = frame_asks(&app);
    let _ = notice(&mut app, "progress build done=65/100 Building aterm");
    assert!(
        app.wire_gate.due().is_none(),
        "PRECONDITION: the restatement past the gap arms no paced paint"
    );
    assert_eq!(
        frame_asks(&app),
        asks,
        "a restatement the wire paints at once asks no window for a frame"
    );
    assert_eq!(
        frame_asks(&app),
        asks_before,
        "the whole flood asks no window for a frame"
    );
}

/// With no Settings view open, a post, a restatement, a resolve and a withdrawal settle the center
/// and ask no window for a frame: no window draws a center row ([`App::sync_messages`]). A due
/// Settings ▸ Messages publish repaints only the windows that show a Settings view
/// ([`App::dispatch_to_settings_views`]), and this fixture opens none.
#[test]
fn a_post_a_resolve_and_a_withdrawal_ask_no_window_for_a_frame() {
    let mut app = App::headless_for_test();
    let asks = frame_asks(&app);
    let resolved = app.post_message(warn("a row a resolve ends"));
    let withdrawn = app.post_message(warn("a row a withdrawal ends"));
    assert_eq!(
        app.messages.live_rows().count(),
        2,
        "PRECONDITION: both rows are live"
    );
    let revision = app.messages.revision();
    assert!(app.restate_message(withdrawn, restatement_of(&warn("a row restated"))));
    assert_ne!(
        app.messages.revision(),
        revision,
        "PRECONDITION: the restatement moved the row, so it synced"
    );
    assert!(
        app.resolve_message(resolved, Outcome::Ok),
        "PRECONDITION: the resolve ended a live row"
    );
    assert!(
        app.withdraw_message(withdrawn),
        "PRECONDITION: the withdrawal ended a live row"
    );
    assert_eq!(
        frame_asks(&app),
        asks,
        "no post, restatement, resolve or withdrawal asks a window for a frame"
    );
}

/// The frame a sync still asks for: the tab strip's waiting fall its settle commits re-grids
/// every window, and the sync asks every window for a frame.
#[test]
fn a_sync_that_lets_the_strip_fall_asks_every_window_for_a_frame() {
    use aterm_messages::chrome::CHROME_REGROW_SETTLE;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(sid));
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .chrome_regrids_for_test = true;
    let t0 = Instant::now()
        .checked_sub(CHROME_REGROW_SETTLE * 2)
        .expect("the clock reads past two floors");
    let _ = app.settle_messages(t0);
    app.set_tab_strip_rows(1, t0);
    app.set_tab_strip_rows(0, t0 + std::time::Duration::from_millis(20));
    assert_eq!(
        app.tab_strip_rows, 1,
        "PRECONDITION: the floor held the fall"
    );
    let asks = crate::redraw_all_requests_on_this_thread();
    let _ = app.post_message(warn("a row whose sync passes the floor"));
    assert_eq!(
        app.tab_strip_rows, 0,
        "the sync's settle let the strip fall"
    );
    assert_eq!(
        crate::redraw_all_requests_on_this_thread(),
        asks + 1,
        "the fall's re-grid asks every window for one frame"
    );
}

/// A post after every sweep of the loop's turn folded its deadline (the status sweep's
/// shell-integration row, a paste wait's end) can land inside the publish gap. The publish it
/// defers is in what the loop arms ([`App::arm_loop_deadline`]), so Settings ▸ Messages shows
/// the row on time, and no frame is asked to wake the loop for it.
#[test]
fn a_publish_a_late_post_defers_is_in_what_the_loop_arms() {
    use crate::app_native::MESSAGES_PUBLISH_MIN_GAP as GAP;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().window_on_screen_for_test = true;
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    let _ = app
        .messages_last_publish
        .expect("opening Settings publishes");
    // The gap runs on the real clock from the last publish. Stamp that publish an hour ahead, so
    // the post's sync falls inside the gap with no race against the time the open took.
    let stamp = Instant::now() + std::time::Duration::from_secs(3600);
    app.messages_last_publish = Some(stamp);
    let asks = crate::redraw_all_requests_on_this_thread();
    let _ = app.post_message(warn("posted by the turn's last sweep"));
    assert_eq!(
        app.messages_last_publish,
        Some(stamp),
        "PRECONDITION: the gap deferred the publish"
    );
    assert_eq!(
        app.messages_publish_due_at(),
        Some(stamp + GAP),
        "PRECONDITION: the deferred publish falls due one gap after the stamp"
    );
    assert_eq!(
        crate::redraw_all_requests_on_this_thread(),
        asks,
        "the post asks no frame that would wake the loop"
    );
    let armed = app.arm_loop_deadline(None, crate::deadline_heal::DeadlineFold::default());
    assert!(
        armed.is_some_and(|d| d <= stamp + GAP),
        "the loop is armed by the deferred publish's due ({:?} after the stamp), armed {armed:?}",
        GAP
    );
}

#[test]
fn notice_act_is_the_pages_press() {
    let mut app = App::headless_for_test();
    let ask = app.post_message(
        Message::new(tags::PRIVACY, Severity::Info, "Allow Full Disk Access?")
            .action(Intent::NotNow {
                decision: Decision::FileAccess,
            })
            .hold(Hold::asking(aterm_messages::HOLD_ASK)),
    );
    let act = |id: MessageId, press: wire::Press| wire::NoticeRequest::Act { id, press };
    assert_eq!(
        app.take_notice(act(ask, wire::Press::Index(0))),
        "OK acted=Not%20now performed=1"
    );
    assert!(app.messages.live(ask).is_none(), "the answer retired it");
    let rec = app.messages.log().get(ask).expect("on record");
    assert_eq!(wire::state_word(rec, None), "answered");
    assert_eq!(
        rec.last_action.as_ref().map(|(l, _)| l.as_str()),
        Some("Not now")
    );
    assert_eq!(
        app.take_notice(act(ask, wire::Press::Index(0))),
        format!("ERR notice: no live message {ask}")
    );
    let open = Intent::OpenSettings {
        route: "/packages".into(),
    };
    let label = open.label();
    let nav = app.post_message(
        Message::new(tags::CONFIG, Severity::Warn, "Tools need attention").action(open),
    );
    assert_eq!(
        app.take_notice(act(nav, wire::Press::Label(label.to_lowercase()))),
        format!(
            "ERR notice: no action {} on message {nav}",
            crate::control::pct_encode(&label.to_lowercase())
        ),
        "labels are exact"
    );
    assert_eq!(
        app.take_notice(act(nav, wire::Press::Label(label.to_string()))),
        format!("OK acted={} performed=1", crate::control::pct_encode(label))
    );
    assert_eq!(
        settings_view(&app, WindowId(0)).map(|v| v.route),
        Some(SettingsRoute::Packages)
    );
    let p = replied_id(
        &notice(&mut app, "progress fetch Fetching sources"),
        "message=",
    );
    assert!(!app.messages.live(p).expect("live").seen);
    let reply = app.take_notice(act(p, wire::Press::Details));
    assert!(
        reply.starts_with(&format!(
            "OK acted={} performed=",
            crate::control::pct_encode(Intent::Details.label())
        )),
        "{reply}"
    );
    assert!(app.messages.live(p).expect("still live").seen);
    assert_eq!(
        app.take_notice(act(p, wire::Press::Index(0))),
        format!("ERR notice: no action 0 on message {p}")
    );
}

#[test]
fn r44_host2_a_reload_that_never_rederived_the_font_family_leaves_its_row() {
    for dismissed in [false, true] {
        let mut app = App::headless_for_test();
        let msg = crate::message_reporters::font_family_rejected(
            "font_family: \"Nope\" is not an admissible font",
        );
        let title = msg.title.clone();
        let id = app.post_message(msg);
        if dismissed {
            assert!(app.messages.dismiss(id, Instant::now()));
            app.sync_messages();
        }
        app.replace_config_messages(Vec::new());
        let rec = app.messages.log().get(id).unwrap();
        assert_eq!(
            (
                app.messages.live(id).is_some(),
                rec.title.as_str(),
                rec.severity
            ),
            (!dismissed, title.as_str(), Severity::Warn),
            "dismissed {dismissed}: {rec:?}"
        );
    }
}

#[test]
fn r44_review_a_reload_whose_font_work_admits_the_family_closes_its_rejection() {
    fn reload(app: &mut App, dir: &std::path::Path, warnings: Vec<String>) {
        let path = dir.join("aterm.toml");
        crate::test_scratch::write_config(&path, "# font_family cleared\n");
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .expect("observe");
        let config = crate::app_config::Config::default();
        let renderer = aterm_render::Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("the embedded face");
        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                values: std::collections::BTreeMap::new(),
                disk_faults: std::sync::Arc::default(),
                assets: config.resolve_asset_catalog(),
                path_feed_fps: config.path_feed_fingerprints(),
                sparkle: config.prepare_sparkle_runtime(),
                fonts: Some(crate::native_font_catalog::PreparedFonts {
                    renderer,
                    family: None,
                    config: app.font_config.clone(),
                    chrome: crate::tray_raster::prepare_chrome_fonts(None, None, None),
                }),
                warnings,
                font_refusals: Vec::new(),
                reason: crate::app_colorscheme::ThemeChange::ConfigReload,
                config,
            },
        );
    }
    let dir = std::env::temp_dir().join(format!("aterm-r44-font-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let rejected = || {
        crate::message_reporters::font_family_rejected(
            "font_family: \"Nope\" is not an admissible font",
        )
    };
    let mut app = App::headless_for_test();
    let id = app.post_message(rejected());
    reload(
        &mut app,
        &dir,
        vec![
            "config font_family: \"Nope\" is not installed; keeping the current working face"
                .to_string(),
        ],
    );
    assert!(
        app.messages.live(id).is_some(),
        "a reload that rejected the family again closed its row"
    );
    let mut app = App::headless_for_test();
    let id = app.post_message(rejected());
    reload(&mut app, &dir, Vec::new());
    let _ = std::fs::remove_dir_all(&dir);
    let rec = app.messages.log().get(id).unwrap();
    assert!(
        app.messages.live(id).is_none() && rec.severity == Severity::Success,
        "the family the reload admitted still reads rejected: {:?} {:?} {:?}",
        rec.title,
        rec.severity,
        rec.state
    );
}

#[test]
fn r44_host3_a_retired_rows_live_intents_are_gone_from_the_page() {
    let a = WindowId(0);
    for intent in [
        Intent::ShowTab {
            tab: 2,
            window: a.0,
        },
        Intent::StopPaste { session: 1 },
        Intent::EndRecovered,
    ] {
        let mut app = App::headless_for_test();
        app.push_stub_tab(a, crate::stub_session(app.next_session_id));
        app.switch_tab_in(a, 0);
        let id = app.post_message(
            Message::new(tags::SYSTEM, Severity::Warn, "a live row").action(intent.clone()),
        );
        assert!(app.withdraw_message(id));
        let rec = app.messages.log().get(id).unwrap();
        assert!(
            !rec.still_offers(&rec.actions[0]),
            "{intent:?}: the engine's rule: no longer offered"
        );
        assert_eq!(
            app.perform_message_act(a, id.raw(), 0),
            MessageActOutcome::Gone,
            "{intent:?}: a retired row's intent was performed from the page"
        );
        assert_eq!(
            app.windows[&a].tab_set.active_index(),
            Some(0),
            "{intent:?}: no tab switched"
        );
    }
}

#[test]
fn a_linux_install_waiting_for_a_start_is_a_notice_and_its_end_takes_it_down() {
    let body = "aterm 0.11.0 was installed over an hour ago and has not started yet: the next \
                aterm you start finishes the update, and update checks wait until then.";
    let mut app = App::headless_for_test();
    app.windows
        .get_mut(&WindowId(0))
        .unwrap()
        .window_on_screen_for_test = true;
    assert!(
        !app.note_update_health(aterm_update::INSTALL_AWAITS_START_TITLE, body),
        "the wait for a start is no health warning"
    );
    let row = app
        .messages
        .live_by_key(update_words::KEY_AWAITS_START)
        .expect("the notice is a live row");
    assert_eq!(row.msg.severity, Severity::Info);
    assert_eq!(row.msg.title, aterm_update::INSTALL_AWAITS_START_TITLE);
    assert!(
        row.msg.detail.iter().any(|line| line.contains("0.11.0")),
        "{:?}",
        row.msg.detail
    );
    assert!(
        app.live_update_health(|_| true).is_empty(),
        "no health warning of any kind"
    );
    assert!(app.update_health_latched.is_empty(), "no kind is latched");
    assert!(
        app.ripening_deadline().is_none(),
        "no digest line is claimed"
    );
    assert!(!app.note_update_health(aterm_update::INSTALL_WAIT_OVER_TITLE, "0.11.0"));
    assert!(
        app.messages
            .live_by_key(update_words::KEY_AWAITS_START)
            .is_none(),
        "its end takes it down"
    );

    let rollback = "aterm 0.11.0 didn\u{2019}t start properly, so aterm 0.10.0 is back and \
                    aterm 0.11.0 is held a while before it is tried again.";
    assert!(app.note_update_health(aterm_update::INSTALL_CRASH_LOOP_TITLE, rollback));
    let install = app.live_update_health(|k| k == update_words::HealthKind::Install);
    assert_eq!(
        install.len(),
        1,
        "the rollback is the install half's warning"
    );
    let row = app.messages.live(install[0]).unwrap();
    assert!(
        row.msg.detail.iter().any(|line| line.contains("is back")),
        "in its own words: {:?}",
        row.msg.detail
    );
}

#[test]
fn a_linux_start_wait_said_unseen_is_shown_once_seen_unless_it_ended() {
    let body = "aterm 0.11.0 was installed over an hour ago and has not started yet: the next \
                aterm you start finishes the update, and update checks wait until then.";
    let secs = std::time::Duration::from_secs;
    let on_screen = |app: &mut App, seen: bool| {
        let ws = app.windows.get_mut(&WindowId(0)).unwrap();
        ws.window_on_screen_for_test = true;
        ws.occluded = !seen;
    };
    let live = |app: &App| {
        app.messages
            .live_by_key(update_words::KEY_AWAITS_START)
            .is_some()
    };

    let mut app = App::headless_for_test();
    on_screen(&mut app, false);
    let said = Instant::now();
    app.note_update_health(aterm_update::INSTALL_AWAITS_START_TITLE, body);
    app.settle_messages(said + secs(10 * 60));
    assert!(live(&app), "it waits while nobody can see it");
    on_screen(&mut app, true);
    app.settle_messages(said + secs(10 * 60 + 1));
    assert!(live(&app), "shown once a window can show it");

    let mut ended = App::headless_for_test();
    on_screen(&mut ended, false);
    let said = Instant::now();
    ended.note_update_health(aterm_update::INSTALL_AWAITS_START_TITLE, body);
    ended.settle_messages(said + secs(10 * 60));
    ended.note_update_health(aterm_update::INSTALL_WAIT_OVER_TITLE, "0.11.0");
    on_screen(&mut ended, true);
    ended.settle_messages(said + secs(10 * 60 + 1));
    assert!(!live(&ended), "a wait that ended is owed nothing");
}

#[test]
fn an_owed_install_warning_is_answered_by_the_landing_it_is_carried_to() {
    let title = aterm_update::health_failing_title("apply");
    let kind = update_words::HealthKind::of_updater_title(title);
    assert_eq!(kind, update_words::HealthKind::Install, "PRECONDITION");
    let body = "build 12 has been waiting on this machine since 2026-10-16T08:00:00Z and \
                build 11 is still running: no apply attempt has been recorded for it.";
    let secs = std::time::Duration::from_secs;
    let on_screen = |app: &mut App, seen: bool| {
        let ws = app.windows.get_mut(&WindowId(0)).unwrap();
        ws.window_on_screen_for_test = true;
        ws.occluded = !seen;
    };
    let records = |app: &App, title: &str| {
        app.messages
            .log()
            .records()
            .filter(|r| r.title == title)
            .count()
    };
    let mut parent = App::headless_for_test();
    on_screen(&mut parent, false);
    let said = Instant::now();
    assert!(parent.note_update_health(title, body), "announced");
    parent.settle_messages(said + secs(10 * 60));
    let warned = parent.live_update_health(|k| k == kind);
    assert_eq!(
        warned.len(),
        1,
        "PRECONDITION: the warning waits unseen in the parent"
    );
    assert!(
        parent.carried_health_said().iter().all(|said| said.owed),
        "PRECONDITION: the carry owes it"
    );

    let wire = aterm_json::to_string(&parent.carried_health_said()).expect("the wire");
    let mut cold = App::headless_for_test();
    on_screen(&mut cold, true);
    cold.take_up_carried_health_said(&crate::app_update_handoff::parse_cold_lane_health_said(
        Some(std::ffi::OsStr::new(&wire)),
    ));
    cold.settle_messages(Instant::now());
    assert!(
        cold.live_update_health(|k| k == kind).is_empty(),
        "the cold successor does not show what its landing answers"
    );
    cold.post_update_landed("9.9.9", 7, 0);
    cold.settle_messages(Instant::now());
    assert_eq!(records(&cold, title), 0, "nor records it");
    assert_eq!(
        records(&cold, "aterm updates work again"),
        1,
        "the landing records the healing"
    );

    parent.post_message(update_words::installing("9.9.9"));
    let carry = window_carry(parent.carried_messages());
    let mut seamless = App::headless_for_test();
    seamless.incoming_handoff_pending = true;
    on_screen(&mut seamless, true);
    seamless.seed_carried_messages(&carry, Some("9.9.9"));
    seamless.settle_messages(Instant::now());
    assert!(
        seamless.live_update_health(|k| k == kind).is_empty(),
        "the seamless successor does not show it under the freeze"
    );
    seamless.incoming_handoff_pending = false;
    seamless.messages.after_handoff_commit(Instant::now());
    seamless.post_update_landed("9.9.9", 7, 0);
    seamless.settle_messages(Instant::now());
    assert!(
        seamless
            .messages
            .log()
            .records()
            .filter(|r| r.title == title)
            .all(|r| warned.contains(&r.id) && !r.is_live()),
        "nor records it again: its one record is the parent's, carried and answered"
    );
    assert_eq!(
        records(&seamless, "aterm updates work again"),
        1,
        "the landing records the healing"
    );
}

/// AN OWED WARNING IS SEEN ONCE A WINDOW IS ON SCREEN AND ENDS AT ITS HOLD: said while no window
/// is on screen, it stays live and owed however long that lasts; once a window is, it counts as
/// seen, the carry owes it no more, and it is restated no more though the window leaves the
/// screen again, so its hold (`HOLD_WARN`) from the window ends it. The Messages page says `live`,
/// then `live for …`, never `waiting to show`.
/// NEGATIVE CONTROL (scratch): restating an owed row's hold on every settle, seen or not, keeps it
/// live past its hold.
#[test]
fn an_owed_warning_is_seen_once_a_window_is_on_screen_and_ends_at_its_hold() {
    let title = aterm_update::health_failing_title("apply");
    let body = "build 12 has been waiting on this machine since 2026-10-16T08:00:00Z and \
                build 11 is still running: no apply attempt has been recorded for it.";
    let secs = std::time::Duration::from_secs;
    let on_screen = |app: &mut App, seen: bool| {
        let ws = app.windows.get_mut(&WindowId(0)).unwrap();
        ws.window_on_screen_for_test = true;
        ws.occluded = !seen;
    };
    let owed = |app: &App| app.carried_health_said().iter().any(|said| said.owed);
    let mut app = App::headless_for_test();
    on_screen(&mut app, false);
    let said = Instant::now();
    assert!(app.note_update_health(title, body), "announced");
    let id = *app
        .live_update_health(|_| true)
        .first()
        .expect("PRECONDITION: the warning is live");
    let words = |app: &App| {
        app.messages_state()
            .entries
            .iter()
            .find(|e| e.id == id.raw())
            .map(aterm_messages::page::MessageView::state_words)
    };
    for at in [10, 70, 10 * 60] {
        app.settle_messages(said + secs(at));
        assert!(
            app.messages.live(id).is_some(),
            "{at} s: it waits while no window is on screen"
        );
        assert!(owed(&app), "{at} s: and is owed");
    }
    assert_eq!(words(&app).as_deref(), Some("live"));
    let seen = said + secs(10 * 60 + 1);
    on_screen(&mut app, true);
    app.settle_messages(seen);
    assert!(!owed(&app), "seen, the carry owes it no more");
    on_screen(&mut app, false);
    let hold = aterm_messages::HOLD_WARN;
    let mut step = secs(1);
    while step < hold {
        app.settle_messages(seen + step);
        assert!(app.messages.live(id).is_some(), "{step:?} into its hold");
        step += secs(1);
    }
    app.settle_messages(seen + hold);
    assert!(
        app.messages.live(id).is_none(),
        "its hold from the window ends it"
    );
    assert!(!owed(&app), "the carry owes it no more");
    let ended = words(&app).expect("its record");
    assert!(ended.starts_with("live for "), "{ended}");
}

/// A SETTINGS TAB CLOSED FROM BEHIND SHOWED NOTHING: `settings close` with a terminal in front
/// leaves the font problem a live row, since Settings never came to the front to show it.
#[test]
fn closing_settings_from_behind_keeps_the_config_row_it_never_showed() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings};
    let scope: Vec<&'static str> = ConfigFamily::ALL.iter().map(|f| f.key()).collect();
    let fonts = ConfigFamily::Fonts.key();
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
    app.switch_tab_in(WindowId(0), 0);
    assert!(!app.settings_in_front());
    let mut warns = ConfigWarnings::default();
    warns.push_shown_in_settings(
        ConfigFamily::Fonts,
        "config font_family: \"NoSuchFontXYZ\" does not resolve to a font file; keeping the \
         current working face"
            .to_string(),
    );
    app.note_config_settings_shows(&scope, &warns.settings_shown_keys());
    let messages = warns.into_messages_routed(app.settings_in_front());
    app.replace_config_messages(messages);
    assert!(
        app.messages.live_by_key(fonts).is_some(),
        "PRECONDITION: over a terminal the problem is a live row"
    );

    assert!(app.close_settings_tabs());
    assert_eq!(
        app.windows[&WindowId(0)].tab_set.len(),
        1,
        "Settings closed"
    );
    assert!(
        app.messages.live_by_key(fonts).is_some(),
        "Settings never came to the front: the row stands"
    );
}

#[test]
fn a_refused_font_size_is_a_record_while_settings_is_in_front() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings};
    let shown = |config_text: &str| {
        let config = crate::app_config::Config::parse(config_text).expect("config");
        let mut warns = ConfigWarnings::default();
        crate::app_config::collect_unaccepted_value_notices(
            &mut warns,
            &config,
            &crate::app_config::ThemeCatalog::empty(),
            config_text,
        );
        warns
    };
    let key = ConfigFamily::UnacceptedValues.key();
    let warns = shown("font_px = 500.0\n");
    assert!(warns.settings_shows(ConfigFamily::UnacceptedValues));
    let messages = warns.into_messages_routed(true);
    let msg = messages
        .iter()
        .find(|m| m.key.as_deref() == Some(key))
        .expect("the refused size is said");
    assert_eq!(msg.hold, Hold::LogOnly, "a record with Settings in front");
    let messages = shown("font_px = 500.0\n").into_messages_routed(false);
    let msg = messages
        .iter()
        .find(|m| m.key.as_deref() == Some(key))
        .expect("the refused size is said");
    assert_ne!(msg.hold, Hold::LogOnly, "a live row over a terminal");

    let mut mixed = shown("font_px = 500.0\n");
    mixed.push(
        ConfigFamily::UnacceptedValues,
        "config harness.idle_minutes: \"soon\" is not a number; keeping 30".to_string(),
    );
    assert!(!mixed.settings_shows(ConfigFamily::UnacceptedValues));
    let messages = mixed.into_messages_routed(true);
    let msg = messages
        .iter()
        .find(|m| m.key.as_deref() == Some(key))
        .expect("said");
    assert_ne!(
        msg.hold,
        Hold::LogOnly,
        "a family with a sentence no row says keeps its live row"
    );
}

#[test]
fn a_live_row_that_becomes_its_record_says_its_sentence_once() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings, config_record};
    let config = crate::app_config::Config::parse("font_px = 500.0\n").expect("config");
    let mut warns = ConfigWarnings::default();
    crate::app_config::collect_unaccepted_value_notices(
        &mut warns,
        &config,
        &crate::app_config::ThemeCatalog::empty(),
        "font_px = 500.0\n",
    );
    let row = warns
        .into_messages_routed(false)
        .into_iter()
        .find(|m| m.key.as_deref() == Some(ConfigFamily::UnacceptedValues.key()))
        .expect("the refused size is said");
    assert!(
        row.detail.len() >= 2 && row.detail[1].starts_with(&row.detail[0]),
        "the live row's first detail line heads its sentence: {:?}",
        row.detail
    );
    let record = config_record(&row);
    assert_eq!(record.hold, Hold::LogOnly);
    assert_eq!(
        record.detail,
        row.detail[1..].to_vec(),
        "the record keeps the sentences alone"
    );
}

#[test]
fn a_fixed_config_problem_is_logged_in_the_words_of_how_it_ended() {
    use crate::app_config::{Config, ConfigAssetCatalog, WallpaperAsset, config_fix_observations};
    use crate::message_reporters::{ConfigFamily, ConfigWarnings, KEY_FONT_FAMILY};
    let catalog = |wallpaper: WallpaperAsset| {
        let mut catalog = (*ConfigAssetCatalog::empty()).clone();
        catalog.wallpaper = wallpaper;
        catalog
    };
    let invalid = catalog(WallpaperAsset::Invalid {
        source_id: "/tmp/bad.png".into(),
        bounded_reason: "not a decodable image".into(),
    });
    let ready = catalog(WallpaperAsset::Ready {
        source_id: "/tmp/good.png".into(),
        w: 1,
        h: 1,
        rgba: std::sync::Arc::from(vec![0u8; 4]),
        fp: 1,
    });
    let none = catalog(WallpaperAsset::None);
    let plain = Config::default();
    let assets = ConfigFamily::Assets.key();
    assert_eq!(
        config_fix_observations(&plain, &invalid, &plain, &none)
            .get(assets)
            .map(String::as_str),
        Some("Wallpaper removed")
    );
    assert_eq!(
        config_fix_observations(&plain, &invalid, &plain, &ready)
            .get(assets)
            .map(String::as_str),
        Some("Wallpaper loaded")
    );
    assert_eq!(
        config_fix_observations(&plain, &none, &plain, &none).get(assets),
        None,
        "nothing refused before: no words decided"
    );
    let named = Config::parse("font_family = \"NoSuchFontXyz\"\n").expect("config");
    assert_eq!(
        config_fix_observations(&named, &none, &plain, &none)
            .get(KEY_FONT_FAMILY)
            .map(String::as_str),
        Some("Font setting removed")
    );
    assert_eq!(
        config_fix_observations(&named, &none, &named, &none)
            .get(KEY_FONT_FAMILY)
            .map(String::as_str),
        Some("Font applied")
    );

    let refused = || {
        let mut warns = ConfigWarnings::default();
        warns.push_shown_in_settings(
            ConfigFamily::Assets,
            "config wallpaper \"/tmp/bad.png\" invalid: not a decodable image".to_string(),
        );
        warns
    };
    for in_front in [false, true] {
        let mut app = App::headless_for_test();
        let messages = refused().into_messages_routed(in_front);
        let id = app.post_message(messages.into_iter().next().expect("one family"));
        let removed = std::collections::BTreeMap::from([(
            assets.to_string(),
            "Wallpaper removed".to_string(),
        )]);
        app.replace_config_messages_after_font_work(Vec::new(), false, &removed);
        let rec = app.messages.log().get(id).expect("logged");
        assert_eq!(
            rec.title, "Wallpaper removed",
            "in_front={in_front}: {rec:?}"
        );
        assert_eq!(rec.severity, Severity::Success, "{rec:?}");
        let mut app = App::headless_for_test();
        let messages = refused().into_messages_routed(in_front);
        let id = app.post_message(messages.into_iter().next().expect("one family"));
        app.replace_config_messages(Vec::new());
        let rec = app.messages.log().get(id).expect("logged");
        assert_eq!(
            rec.title, "Image problem fixed",
            "in_front={in_front}: {rec:?}"
        );
    }
}

#[test]
fn a_tools_later_entry_is_one_digest_entry_its_link_press_files() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-later-cause-")
        .tempdir()
        .unwrap();
    let path = dir.path().join(crate::messages_store::FILE_NAME);
    let within = std::time::Duration::from_secs(60);
    let line = "later codex.security-setup.0123abcd tag=codex \
                url=https://chatgpt.com/security/setup Set up Codex account security -- \
                only you can enrol a second factor for your account -- \
                Finish the setup in your browser";
    let req = || wire::NoticeRequest::parse(line).expect("a later line");
    let mut app = App::headless_for_test();
    app.messages_log = crate::messages_store::Writer::spawn(&path);
    assert!(app.messages_log.is_some(), "the owned journal opened");
    let first = app.take_notice_within(req(), within);
    assert!(
        first.starts_with("OK later=") && first.ends_with(" how=new"),
        "{first}"
    );
    let again = app.take_notice_within(req(), within);
    assert!(again.ends_with(" how=duplicate"), "{again}");
    assert_eq!(app.messages.wanted_rows(), 0, "never a revealed row");
    let digest = app.messages.digest();
    assert_eq!(digest.len(), 1);
    let entry = &digest.entries[0];
    assert_eq!(
        entry.actions,
        [Intent::OpenLink {
            // Distinct branch.
            url: "https://chatgpt.com/security/setup".into()
        }]
    );
    assert!(
        entry
            .detail
            .contains(&"Opens a page on chatgpt.com".to_string()),
        "{:?}",
        entry.detail
    );
    let id = entry.id;
    assert_eq!(
        app.perform_message_act(WindowId(0), id.raw(), 0),
        MessageActOutcome::Refused {
            feedback: "Could not open the page".to_string()
        },
        "a headless instance opens no browser"
    );
    assert!(app.messages.digest().is_empty(), "the press filed it");
    assert!(matches!(
        app.messages.log().get(id).and_then(LogRecord::retired),
        Some(Retired::Answered { .. })
    ));
    let next_tab = app.take_notice_within(req(), within);
    assert!(next_tab.ends_with(" how=quiet"), "{next_tab}");
    app.flush_messages_log();
    let filed = crate::messages_store::load_tail(&path, u64::MAX);
    assert_eq!(filed.skipped, 0);
    assert!(matches!(
        filed.log.get(id).and_then(LogRecord::retired),
        Some(Retired::Answered { .. })
    ));
    let mut replayed = aterm_messages::MessageCenter::new(filed.log, Instant::now());
    let quiet = wire::apply(
        &mut replayed,
        &mut wire::WireGate::default(),
        req(),
        wall_stamp_now(),
        Instant::now(),
    );
    assert!(quiet.reply.ends_with(" how=quiet"), "{}", quiet.reply);

    let revision = app.messages.projection_revision();
    let done = app.take_notice_within(
        wire::NoticeRequest::parse("done codex.security-setup.0123abcd").unwrap(),
        within,
    );
    assert_eq!(done, "OK done=- how=gone");
    assert_ne!(app.messages.projection_revision(), revision);
    let ended = crate::messages_store::load_tail(&path, u64::MAX);
    assert_eq!(ended.skipped, 0);
    assert_eq!(
        ended.log.get(id).and_then(LogRecord::retired),
        Some(&Retired::Resolved(Outcome::Ok))
    );
    let mut replayed = aterm_messages::MessageCenter::new(ended.log, Instant::now());
    let new_occurrence = wire::apply(
        &mut replayed,
        &mut wire::WireGate::default(),
        req(),
        wall_stamp_now(),
        Instant::now(),
    );
    assert!(
        new_occurrence.reply.ends_with(" how=new"),
        "{}",
        new_occurrence.reply
    );
    drop(app.messages_log.take());
}

#[test]
fn open_in_browser_opens_only_an_https_page() {
    assert!(openable_link("https://chatgpt.com/security/setup?x=1#a"));
    for bad in [
        "http://chatgpt.com/",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "mailto:someone@chatgpt.com",
        "https://someone@chatgpt.com/",
        "https://chatgpt.com:8443/",
        "https://chatgpt.com/\n",
        "https://chatgpt.com/a b",
        " https://chatgpt.com/",
        "open-link:https://chatgpt.com/",
    ] {
        assert!(!openable_link(bad), "{bad:?}");
    }
}

#[test]
fn automatic_reports_are_records_without_banners_or_a_user_action() {
    let mut app = App::headless_for_test();
    let id = app.post_message(warn("Automatic work failed"));
    let log = app.messages.log().get(id).expect("diagnostic retained");
    assert_eq!(log.title, "Automatic work failed");
    assert!(app.restate_message(
        id,
        Restatement {
            title: Some("Another approach succeeded".into()),
            ..Restatement::default()
        }
    ));
    assert!(app.resolve_message(id, Outcome::Ok));
    assert_eq!(
        app.messages.log().get(id).unwrap().title,
        "Another approach succeeded"
    );
}

/// A live update download with an amount: a determinate bar with an ETA slot.
fn metered_download(done: u64) -> Message {
    use aterm_messages::{Amount, Meter, STALE_UPDATE, Unit};
    let total = 74_000_000;
    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.92.0")
        .meter(Meter {
            fill_permille: Some(u16::try_from(done * 1000 / total).unwrap()),
            stats: String::new(),
            amount: Some(Amount {
                series: Amount::series_of("aterm 0.92.0"),
                done,
                total,
                unit: Unit::Bytes,
            }),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: STALE_UPDATE,
        })
        .key("update.progress")
}

/// `appstatus` output is invariant across time: its rows carry no motion words.
#[test]
fn appstatus_carries_no_motion_words() {
    let mut app = App::headless_for_test();
    app.post_message(metered_download(30_000_000));
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let read = |app: &App, at: Instant| {
        aterm_messages::wire::activity_rows_compat(&app.messages, at, &crate::control::pct_encode)
    };
    let now = Instant::now();
    let first = read(&app, now);
    assert_eq!(first.len(), 2, "{first:?}");
    assert_eq!(read(&app, now + std::time::Duration::from_secs(30)), first);
}

/// `Hold::LogOnly` messages stay off the glass and appear in Settings ▸ Messages newest first.
#[test]
fn the_messages_page_lists_log_only_records() {
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
    ws.window_on_screen_for_test = true;
    ws.focused = true;
    let first = app.record_message(Message::new(
        tags::TOOLCHAIN,
        Severity::Info,
        "ALab tools installed",
    ));
    let second = app.record_message(
        Message::new(tags::UPDATE, Severity::Warn, "aterm update failed")
            .line("could not verify the download"),
    );
    assert_eq!(app.messages_deadline(), None);
    let state = app.messages_state();
    let ids: Vec<u64> = state.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, [second.raw(), first.raw()], "newest first");
    for id in [first, second] {
        let e = state.entry(id.raw()).expect("listed");
        assert_eq!(e.state, "recorded");
    }
    assert_eq!(
        state.entry(second.raw()).unwrap().detail,
        vec!["could not verify the download"],
        "its detail rides the page"
    );
}

/// A tab strip fall the regrow floor held commits when a settle passes the floor, and that
/// settle asks for a repaint: the strip's re-grid is drawn though no message moved.
#[test]
fn a_strip_fall_the_floor_held_is_redrawn_when_a_settle_takes_it() {
    use aterm_messages::chrome::CHROME_REGROW_SETTLE;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(sid));
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .chrome_regrids_for_test = true;
    let t0 = Instant::now();
    let _ = app.settle_messages(t0);
    app.set_tab_strip_rows(1, t0);
    app.set_tab_strip_rows(0, t0 + std::time::Duration::from_millis(20));
    assert_eq!(
        app.tab_strip_rows, 1,
        "PRECONDITION: the floor holds the fall"
    );
    let _ = crate::app_presence::take_chrome_regrids_applied();
    assert!(
        !app.settle_messages(t0 + CHROME_REGROW_SETTLE / 2),
        "inside the floor: nothing to draw"
    );
    assert_eq!(app.tab_strip_rows, 1, "still held");
    assert!(
        app.settle_messages(t0 + CHROME_REGROW_SETTLE),
        "the fall's re-grid asks for a repaint"
    );
    assert_eq!(app.tab_strip_rows, 0, "the strip fell");
    assert!(
        crate::app_presence::take_chrome_regrids_applied()
            .iter()
            .any(|(w, rows)| *w == wid && *rows == app.chrome_rows(wid)),
        "the window re-gridded to the fallen strip"
    );
}
