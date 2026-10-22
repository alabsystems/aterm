// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::update_words::LiveHealth as _;
#[cfg(unix)]
use aterm_messages::MAX_ROWS;
use aterm_messages::{LogRecord, LogState, Retired, Severity, tags};

fn warn(title: &str) -> Message {
    Message::new(tags::CONFIG, Severity::Warn, title).line("why")
}

use crate::toolchain_words::held_row;

/// A settle this far past a resolve commits the band's quiet shrink: the later of
/// [`aterm_messages::SHRINK_QUIET`] and the longest echo (a quiet shrink waits for every
/// echo to end), plus a millisecond.
fn quiet_past_echoes() -> std::time::Duration {
    let longest = aterm_messages::animate::echo_span(aterm_messages::EchoKind::Complete, 0, true);
    aterm_messages::SHRINK_QUIET.max(longest) + std::time::Duration::from_millis(1)
}

/// The Settings view fronting window `wid`, if Settings fronts it.
fn settings_view(app: &App, wid: WindowId) -> Option<&crate::native_settings::SettingsViewState> {
    let (_, view) = app.active_native_view(wid)?;
    match app.native_runtime.view_state(view)? {
        crate::native_app::AppViewState::Settings(state) => Some(state),
        _ => None,
    }
}

/// The idle center arms nothing (FL-1); a posted row arms its fold; a
/// handoff freeze suspends the holds — and the deadline agrees with the
/// settle it arms, both reading the one predicate. Mid-handoff the row
/// count never moves (the successor's layout must match the parent's
/// capture) until the freeze lifts.
#[test]
fn the_deadline_is_none_when_idle_and_the_holds_freeze_under_a_handoff() {
    let mut app = App::headless_for_test();
    assert_eq!(app.messages_deadline(), None, "an idle band never wakes");
    let id = app.post_message(warn("a warning"));
    assert!(app.messages.live(id).is_some());
    assert_eq!(
        app.message_band_rows, 1,
        "a headless App has no window to clamp against: the row commits"
    );
    assert!(app.messages_deadline().is_some(), "the hold is armed");
    app.incoming_handoff_pending = true;
    assert!(app.message_holds_frozen());
    assert_eq!(
        app.messages_deadline(),
        None,
        "a held row's fold is not a wake while the holds are frozen"
    );
    let frozen = app.post_message(warn("frozen"));
    assert!(app.messages.live(frozen).is_some(), "the row is live");
    assert_eq!(app.message_band_rows, 1, "…but no row was committed");
    assert!(
        !app.sync_message_band_rows(Instant::now()),
        "and none will be, mid-handoff"
    );
    // A live row's staleness cap still wakes the loop mid-freeze.
    let live = app.post_message(
        Message::new(tags::UPDATE, Severity::Info, "downloading").hold(Hold::Live {
            stale_after: std::time::Duration::from_secs(30),
        }),
    );
    assert!(app.messages.live(live).is_some());
    assert!(app.messages_deadline().is_some(), "staleness keeps running");
    app.incoming_handoff_pending = false;
    assert!(!app.message_holds_frozen());
    assert!(
        app.sync_message_band_rows(Instant::now()),
        "the freeze lifted: the row commits"
    );
    assert!(app.message_band_rows > 1);
}

/// A shrink pending when a handoff freezes the band is not a wake: only `commit_rows`
/// takes it and the freeze skips `commit_rows`, so an armed shrink would stay due for the
/// whole park → proof → exec window.
#[test]
fn a_frozen_band_never_arms_its_pending_shrink() {
    let mut app = App::headless_for_test();
    let id = app.post_message(warn("a warning"));
    assert_eq!(app.message_band_rows, 1);
    let before = Instant::now();
    assert!(app.withdraw_message(id));
    let after = Instant::now();
    assert_eq!(app.message_band_rows, 1, "the shrink waits out its quiet");
    let shrink = before + aterm_messages::SHRINK_QUIET..=after + aterm_messages::SHRINK_QUIET;
    assert!(
        app.messages.deadline(true).is_some(),
        "unfrozen, the shrink (or the echo) is armed"
    );
    app.incoming_handoff_pending = true;
    assert!(
        app.messages_deadline().is_none_or(|d| !shrink.contains(&d)),
        "frozen, the shrink is not a wake"
    );
    app.incoming_handoff_pending = false;
}

/// The inbox drains into the center with the stamp each message was
/// queued under; the drained lines reach the log ring.
#[test]
fn the_pre_app_inbox_drains_into_the_center_with_its_own_stamp() {
    let _lane = crate::message_inbox::lane_test_guard();
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
fn finished_workstation_setup_clears_existing_and_queued_exceptions() {
    let _lane = crate::message_inbox::lane_test_guard();
    let _ = crate::message_inbox::take_queued();
    let _ = crate::message_inbox::take_workstation_setup_finished();
    let mut app = App::headless_for_test();
    let account = crate::message_reporters::workstation_accounts_entry();
    let settings = crate::message_reporters::workstation_setup_failure_entry();
    let other = crate::message_reporters::devtools_entry(atpkg::devtools::Step::Add);
    app.except(&account);
    app.except(&settings);
    app.except(&other);
    assert!(app.owes_exception(&account.key()));
    assert!(app.owes_exception(&settings.key()));
    crate::report_workstation_setup();
    crate::report_workstation_setup_failure();
    crate::report_workstation_setup_finished();
    app.drain_message_inbox();
    assert!(!app.owes_exception(&account.key()));
    assert!(!app.owes_exception(&settings.key()));
    assert!(
        app.owes_exception(&other.key()),
        "unrelated privacy entries remain"
    );

    // Completion must also reach the center when there are no queued messages.
    app.except(&settings);
    assert!(app.owes_exception(&settings.key()));
    crate::report_workstation_setup_finished();
    app.drain_message_inbox();
    assert!(!app.owes_exception(&settings.key()));
}

/// TORN FABRIC STATE FILES ARE A FACT FOR LATER (finding F1): the bridge's
/// `link set-aside files=<n>`, handed to the store as the fabric's registry
/// ([`crate::fabric::Registry::torn_state_set_aside`]), is queued on the
/// endpoint's thread and raised at the next park as ONE exceptions entry
/// (`fabric.state.torn`, the event family `FabricState`) whose words pass the
/// rule — never a band row; a later count restates the same entry, never a
/// second one.
#[test]
fn torn_fabric_state_set_aside_is_one_exceptions_entry_at_the_next_park() {
    use crate::fabric::Registry as _;
    let _lane = crate::message_inbox::lane_test_guard();
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

/// THE TORN-FILES ENTRY ENDS WHEN THE OWNER EMPTIES THE QUARANTINE (sim item
/// S6): the bridge's `link quarantine torn=<k>`, handed to the store as the
/// fabric's registry ([`crate::fabric::Registry::torn_state_held`]). A count
/// above 0 leaves the entry open; 0 ends it at the next park, `Resolved` —
/// gone from the digest, never a row. And one the owner had dismissed is
/// forgotten at 0, so the next power cut's set-aside is news again.
/// NEGATIVE CONTROL: dismissed and never emptied, the next set-aside stays
/// quiet inside the dismissal's span — the path an entry with no end had.
#[test]
fn torn_fabric_state_ends_once_the_quarantine_holds_none_of_it() {
    let _lane = crate::message_inbox::lane_test_guard();
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

    // OPEN: a count above 0 changes nothing; 0 ends it.
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

    // THE LAST WORD IN ONE PARK WINS: raised then emptied before the park
    // is ended; emptied then raised is open.
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

    // DISMISSED, then emptied, then a later set-aside: news again.
    // NEGATIVE CONTROL first: dismissed and never emptied, it stays quiet.
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

/// THE TORN-FILES ENTRY PROMISES AN END ONLY WHERE ITS BRIDGE WILL SAY ONE
/// (sim item S6, the review's finding on its words): a bridge whose `hello`
/// lane says `link quarantine` gets the line that the entry ends once the
/// quarantine is empty; one from an older aterm never says it, so its entry
/// says to dismiss it instead, and never that it ends on its own. Both pass
/// the rule and are filed under one key, which a later bridge's `torn=0`
/// ends either way.
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

/// The start that fell back to the home folder for `dir`, said as
/// `spawn_session` says it once the shell is up.
fn fell_back(dir: &str, fault: crate::spawn_folder::Fault) {
    crate::spawn_folder::StartFolder {
        cwd: Some("/Users//me".into()),
        fell_back: Some((dir.into(), fault)),
    }
    .say();
}

/// Folders fresh shells could not start in fold into one row per fault: a restore's two
/// gone folders are one row, a third joins it while it is up, a folder not entered is
/// its own row, and after a dismiss the next folder is a fresh row naming only itself.
#[test]
fn folders_that_were_not_found_fold_into_one_row_while_it_is_up() {
    use crate::spawn_folder::Fault;
    let _lane = crate::message_inbox::lane_test_guard();
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

/// A ROW CARRIED ACROSS AN UPDATE KEEPS ITS FOLDERS: the successor's list
/// starts empty while the carried row is up, and the next folder joins
/// the folders that row named instead of replacing them.
#[test]
fn a_carried_folder_row_keeps_its_folders_when_the_next_one_joins() {
    use crate::spawn_folder::{Fault, Named};
    let _lane = crate::message_inbox::lane_test_guard();
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

/// A reload's set REPLACES the config rows: the old family rows are
/// resolved, a family still in the same words stands untouched, and a row
/// keyed elsewhere is never touched.
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

    // The reload fixed the chord and left the typo: the chord's row is resolved, and
    // the typo's (the same words) stands untouched: no resolve, no second post.
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

/// A reload runs on every Settings patch, Presence toggle and Serious Mode write, so once
/// a config row has left the glass (its hold ran out, or it was read) the same words are
/// a record, not a new row. A changed problem raises its row; a fixed one that comes back
/// is news again.
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
    // The person reads it: it folds.
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
    // Three unrelated toggles: the same words each time — records only.
    for _ in 0..3 {
        app.replace_config_messages(typo(3, "windw_padding"));
        assert!(
            app.messages.live_by_key(key).is_none(),
            "a repeat never comes back to the glass"
        );
    }
    assert_eq!(records(&app), before + 3, "…and the log keeps each one");
    assert_eq!(
        app.messages.wanted_rows(),
        0,
        "the band wants no row for it"
    );
    // A changed problem is news: its row is raised.
    app.replace_config_messages(typo(5, "colums"));
    let changed = app.messages.live_by_key(key).expect("a new typo's row");
    assert!(changed.msg.detail.iter().any(|d| d.contains("colums")));
    // Fixed, then broken again the same way: the fix resolved the row, so
    // the words coming back are news again.
    app.replace_config_messages(Vec::new());
    assert!(app.messages.live_by_key(key).is_none(), "fixed");
    app.replace_config_messages(typo(5, "colums"));
    let again = app
        .messages
        .live_by_key(key)
        .map(|l| l.id)
        .expect("a problem that came back after its fix is raised again");
    // …and when the row had already left the glass when the fix landed: news again
    // too.
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

/// A content-equal reload re-derives only the two text-read families, so its replace
/// resolves only those (and the launch load failure, which a file that now parses
/// answers): a still-true keybinding row survives a comment-only save.
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

    // The typo fixed by a save that parses equal: its row and the load
    // failure come down; the chord's row, not re-derived, stands.
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

/// When a family's words change while it is still broken, the old row is superseded and
/// its record keeps its title and ⚠, never a `… fixed` title under a ✓. Only the reload
/// that takes the family away logs the fix's words.
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

    // One chord fixed, the other still skipped.
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
    // Settings ▸ Messages lists it as part of the new row, which keeps the
    // problem in Problems.
    let page = app.messages_state();
    assert_eq!(page.standing_for(two.raw()), Some(one.raw()));
    assert!(
        page.entry(one.raw()).is_some_and(|entry| entry.alarm()),
        "the problem stays in Problems, as the new row"
    );

    // The last chord fixed: the family is gone, and now it is fixed.
    app.replace_config_messages(Vec::new());
    assert!(app.messages.live_by_key(key).is_none());
    let rec = app.messages.log().get(one).expect("logged");
    assert_eq!(rec.retired(), Some(&Retired::Resolved(Outcome::Ok)));
    assert_eq!(rec.severity, Severity::Success, "{rec:?}");

    // A typo joined by a second: the first is still misspelled.
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

/// A retired spelling is a record on every path: `collect_key_notices` puts
/// `[packages] auto_update = false` in `ConfigFamily::RetiredKeys` and the replace logs it
/// without a row, so a comment-only save never puts it on glass. The typo beside it keeps
/// its row.
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

/// THE FINAL READ LEAVES A HELD ROW ALONE (ruling 19). Since the silent
/// lane the `installed` words are a record, so a HELD row on the pass key
/// is one carried across a handoff from a build before it — stood in for
/// here by the record's words at the default hold. The tailer's final
/// snapshot of the ended pass changes nothing on it, first run or not: no
/// meter, no words, no revision, no log line — and the child's exit, which
/// folds the pass key's live row, leaves a held one standing.
#[test]
fn the_final_snapshot_leaves_a_held_row_alone() {
    let mut app = App::headless_for_test();
    let id = app.post_message(held_row(&toolchain_words::installed("claude, codex")));
    {
        let live = app.messages.live(id).expect("held on glass");
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
    let live = app.messages.live(id).expect("still on glass, still held");
    assert_eq!(live.revision, 0, "not restated");
    assert_eq!(live.msg.meter, None, "no meter, no 100%");
    assert_eq!(app.messages.log().len(), 1, "the read posted nothing");
}

/// `Show tab N` from ANOTHER window's band (ruling 247): the band is
/// app-wide, so every window's band shows the row and its capsule.
/// The press lands on the window the row named — its tab 2 selected —
/// and never switches the pressing window to ITS tab 2, a different
/// program. A gone tab or window is an honest miss.
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
    // The row is not restated: its snapshot ordinal now names another tab.
    app.move_tab(a, 1, 0);
    app.switch_tab_in(a, 1);
    assert!(app.perform_intent(b, id, show.clone()));
    assert_eq!(
        active(&app, a),
        Some(0),
        "the target moved, its identity did not"
    );
    assert_eq!(app.focused_session_id(a), Some(target));
    // Removing a preceding tab also changes the ordinal without changing the target.
    app.move_tab(a, 0, 1);
    app.close_tab_at(a, 0);
    assert_eq!(app.windows[&a].tab_set.len(), 1);
    assert!(app.perform_intent(b, id, show.clone()));
    assert_eq!(active(&app, a), Some(0));
    // The target ends, while a different tab occupies its former position.
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
    // The original gone-ordinal/window controls remain refusals too.
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
    // Old carries are decoded but never restamped from their ordinal.
    let legacy = Intent::decode("show-tab:1:0").expect("readable legacy intent");
    assert!(!app.perform_intent(b, id, legacy));
    assert_eq!(active(&app, a), before);
}

/// THE DETAILS PRESS (design §3.6): the row is seen — a held row folds,
/// the config band's "click at a notice you have read", and a standing row
/// too — and Settings ▸ Messages opens in THE PRESSED WINDOW with that
/// entry selected, the page already showing the record as folded. (A live
/// row through the band's own press: `app_mouse`'s
/// `a_details_press_opens_the_entry_in_the_clicked_window`.)
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
    // A standing row folds once read too.
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
    // A plain open (the menu's) leaves the page as it was.
    assert!(app.open_messages_entry(wid, None));
    let view = settings_view(&app, wid).unwrap();
    assert_eq!(view.route, SettingsRoute::Messages);
    assert_eq!(
        view.messages_selected,
        Some(standing.raw()),
        "a plain open leaves the page as it was"
    );
    // A window that does not exist opens nothing.
    assert!(!app.open_messages_entry(WindowId(77), Some(standing)));
}

/// A "To look at" menu line lands on its entry: a page open on another selection moves
/// to it, and with no frontmost window the first window shows it.
#[test]
fn a_menu_line_opens_messages_at_its_entry() {
    let mut app = App::headless_for_test();
    let wid = *app.windows.keys().next().expect("a window");
    app.frontmost_window = None;
    let first = app.post_message(warn("first"));
    let second = app.post_message(warn("second"));
    assert!(app.open_messages_entry(wid, Some(first)));
    assert_eq!(app.open_messages_entry_anywhere(second.raw()), Some(wid));
    let view = settings_view(&app, wid).expect("Settings in the first window");
    assert_eq!(view.route, SettingsRoute::Messages);
    assert_eq!(view.messages_selected, Some(second.raw()));
}

/// THE PROJECTION'S HOST FACTS (design §4.2; the projection itself — order,
/// chips, the wire's words, a decision answered once — is the engine's,
/// `aterm_messages::page_tests`): `Open log` is pressable only while its file
/// is there, `Install now` only while that build is staged (a test App stages
/// nothing), and a navigation always.
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

/// THE PAGE'S PRESS (`perform_message_act`; a live row's press through the
/// engine's `act`, a decision answered once, is `notice_act_is_the_pages_press`,
/// which goes through it): a retired record re-offers its navigation from the
/// log, performed in the pressing window; an unknown id or button is `Gone`;
/// a refusal is `Refused`.
#[test]
fn the_pages_press_acts_through_the_engine_and_words_the_outcome() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // A retired navigation, re-offered from the log.
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
    // A refusal is a refusal.
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

/// REVIEW (ruling 267): the page's status line (`Copied`) is
/// timed by the projections, and on a quiet page those came once a
/// minute — so it stood one to two minutes, never about five seconds. The
/// host now arms the publish that starts its clock and, five seconds on,
/// the one that clears it; then only the minute tick again (the page dates
/// a record stamped now, `just now` until the minute).
#[test]
fn a_transient_status_line_is_cleared_about_five_seconds_on() {
    use crate::app_native::{
        MESSAGES_FEEDBACK_WAIT as WAIT, MESSAGES_PUBLISH_MIN_GAP as GAP,
        MESSAGES_PUBLISH_TICK as TICK,
    };
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
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
    // The publish that starts the clock is due at the 2 Hz gap.
    assert_eq!(app.messages_publish_due_at(), Some(opened + GAP));
    app.publish_native_messages_state_if_due(opened + GAP);
    let stamped = app.messages_last_publish.unwrap();
    assert_ne!(stamped, opened);
    assert_eq!(settings_view(&app, wid).unwrap().status_clock(), Some(true));
    assert!(settings_view(&app, wid).unwrap().feedback.is_some());
    // The one that clears it, five seconds on — not the minute tick.
    assert_eq!(app.messages_publish_due_at(), Some(stamped + WAIT));
    assert!(app.messages_deadline().is_some_and(|d| d <= stamped + WAIT));
    app.publish_native_messages_state_if_due(stamped + WAIT - std::time::Duration::from_millis(1));
    assert_eq!(app.messages_last_publish, Some(stamped), "not yet");
    // The page times the line by the projections' wall clock, which a test
    // cannot wait out: age the stamp as five seconds would.
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

/// A COMPLETION'S LINE IS CLEARED ABOUT FIVE SECONDS ON, ON EVERY PAGE (xa-13): only
/// the Messages page had a clock, so "Build information copied" on About or
/// "Discarded 2 unsaved Settings drafts." on Terminal stood until navigation, the
/// bar's room out of a compact list. The host now arms the same two publishes for
/// a transient line on any page, and nothing after it is cleared (About has no
/// minute tick). NEGATIVE CONTROL: before the line, About arms no publish.
#[test]
fn a_transient_status_line_clears_on_every_page() {
    use crate::app_native::{MESSAGES_FEEDBACK_WAIT as WAIT, MESSAGES_PUBLISH_MIN_GAP as GAP};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
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

/// THE PUBLISH IS RATE-LIMITED, AND ONLY WHILE SETTINGS EXISTS (design
/// §4.2): with no Settings controller a publish — asked for outright or
/// on the sweep — is a no-op and nothing is pending; opening Settings
/// publishes at once; a BURST inside the 2 Hz gap (posts, a resolve) is
/// held back whole — the page keeps showing the last publish — and ONE
/// publish at the gap carries all of it, after which nothing of the
/// center's is pending (only the minute tick, the view being on the
/// route); closing the last Settings tab drops the controller, after
/// which the center moving is nobody's business again; reopening
/// publishes at once, gap or no gap.
#[test]
fn publish_is_rate_limited_and_only_while_settings_exists() {
    use crate::app_native::MESSAGES_PUBLISH_MIN_GAP as GAP;
    use crate::app_native::MESSAGES_PUBLISH_TICK as TICK;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let settings_exists = |app: &App| {
        app.native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_some()
    };
    app.post_message(warn("before Settings"));
    // No controller: nothing to publish into, and nothing pending.
    app.publish_native_messages_state();
    app.publish_native_messages_state_if_due(std::time::Instant::now() + GAP);
    assert!(!settings_exists(&app));
    assert_eq!(app.messages_last_publish, None, "no Settings, no publish");
    assert_eq!(app.messages_published_revision, 0);
    assert_eq!(app.messages_publish_due_at(), None);

    // Settings opens, at the Messages route: published at once.
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    assert!(settings_exists(&app));
    let opened = app
        .messages_last_publish
        .expect("opening Settings publishes");
    let published = app.messages_published_revision;
    assert_eq!(published, app.messages.revision());

    // A burst inside the gap — four posts and a resolve — is held back
    // whole: the page keeps the last publish, the pending one is due at
    // the gap, and the loop is asked back for it.
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

    // Closing the last Settings tab drops the controller: the center
    // moving is nobody's business — no publish, nothing pending.
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

    // Reopened: published at once, whatever the gap would say.
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    assert_ne!(app.messages_last_publish, Some(second));
    assert_eq!(app.messages_published_revision, app.messages.revision());
}

/// The projection republishes every 60 s while a Settings view is on the Messages route,
/// so relative times tick; `messages_deadline` must arm that tick as a wake, or an idle
/// window on the page is never woken and `3 min ago` stays `3 min ago`. With a standing
/// row (the center arms nothing) and a view on the route, the deadline is the tick.
#[test]
fn the_minute_tick_arms_a_wake_while_a_view_is_on_the_route() {
    use crate::app_native::MESSAGES_PUBLISH_TICK as TICK;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    app.post_message(
        Message::new(tags::UPDATE, Severity::Warn, "checks failing").hold(Hold::STANDING),
    );
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    let published = app
        .messages_last_publish
        .expect("opening Settings publishes");
    assert_eq!(app.messages_published_revision, app.messages.revision());
    assert_eq!(
        settings_view(&app, wid).map(|v| v.route),
        Some(SettingsRoute::Messages)
    );
    assert_eq!(
        app.messages.deadline(true),
        None,
        "a standing row arms no fold: the center is quiet"
    );
    assert_eq!(
        app.messages_deadline(),
        Some(published + TICK),
        "the tick must be armed as a wake while a view is on the route"
    );
    // A page nobody can see (the window occluded, minimized or headless) arms no tick;
    // revealed, it arms again.
    app.windows.get_mut(&wid).unwrap().occluded = true;
    assert_eq!(app.messages_deadline(), None, "a hidden page is no wake");
    app.windows.get_mut(&wid).unwrap().occluded = false;
    assert_eq!(app.messages_deadline(), Some(published + TICK));
}

/// A MESSAGES PAGE'S CLOCK IS OWED A PUBLISH ONLY WHEN ITS TIME WORDS MOVE (r8
/// D2). The minute tick armed for every shown page on the route, and since a
/// headless window's active tab counts as shown (xa-13) a headless instance
/// resting on Messages woke every minute even with nothing on the page that
/// could move. Now, on a headless instance (no glass, nothing forced on
/// screen): an empty page says no time and arms nothing; a record three hours
/// old arms the publish at its next hour, not a wake a minute; a record
/// stamped now arms the minute that turns it `1 min ago`; and the page off the
/// route, or Settings in a background tab, arms nothing. NEGATIVE CONTROL: the
/// minute tick armed at the empty page and at the three-hour-old record.
#[test]
fn a_headless_messages_page_is_republished_only_when_its_time_words_move() {
    use crate::app_native::{MESSAGES_PUBLISH_MIN_GAP as GAP, MESSAGES_PUBLISH_TICK as TICK};
    const MIN: u64 = 60_000;
    let mut app = App::headless_for_test();
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

    // A record from three hours and ten minutes ago: `3 h ago` until four.
    let now = wall_stamp_now().unix_ms;
    // Records, never rows: the band arms no wake of its own.
    app.post_message_at(
        warn("an old record").hold(Hold::LogOnly),
        aterm_messages::WallStamp {
            unix_ms: now - 190 * MIN,
        },
    );
    app.publish_native_messages_state_if_due(opened + GAP);
    let published = app.messages_last_publish.unwrap();
    assert_ne!(published, opened);
    let until = app
        .messages_words_change_in
        .expect("a record's age is said");
    assert!(
        until > TICK && until <= std::time::Duration::from_millis(50 * MIN),
        "its next hour, not the minute: {until:?}"
    );
    assert_eq!(app.messages_publish_due_at(), Some(published + until));

    // A record stamped now: `just now` turns `1 min ago` within the minute,
    // which is as soon as the page is republished for its clock.
    app.post_message(warn("a new record").hold(Hold::LogOnly));
    app.publish_native_messages_state_if_due(published + GAP);
    let fresh = app.messages_last_publish.unwrap();
    assert!(
        app.messages_words_change_in
            .is_some_and(|until| until <= TICK)
    );
    assert_eq!(app.messages_publish_due_at(), Some(fresh + TICK));

    // Off the route, nothing; back on it, the clock again.
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

    // Settings behind the terminal tab is not what the instance shows.
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

/// THE PAGE'S CLOCK COUNTS THE NIGHT THE MAC SLEPT, AND THE ZONE IT IS IN.
/// A page is republished when its time words move, a span after the publish
/// on the monotonic clock, which stops while a Mac sleeps: a page that said
/// `3 h ago` and slept the night would say it for most of an awake hour more,
/// and `Today` past a midnight it slept through. The first turn after the
/// sleep takes the time slept off the span, so the page is owed its publish
/// (never sooner than a minute after the last); a wall clock stepped back
/// moves nothing. A zone that reads another offset than the words were read
/// on owes the publish too, and the publish reads the zone again. NEGATIVE
/// CONTROL: uncounted, the slept page waits out the rest of its hour.
#[test]
fn a_messages_page_that_slept_through_its_words_is_owed_its_publish_on_waking() {
    use crate::app_native::MESSAGES_PUBLISH_TICK as TICK;
    const MIN: u64 = 60_000;
    const NIGHT: std::time::Duration = std::time::Duration::from_secs(8 * 3600);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = wall_stamp_now().unix_ms;
    app.post_message_at(
        warn("an old record").hold(Hold::LogOnly),
        aterm_messages::WallStamp {
            unix_ms: now - 190 * MIN,
        },
    );
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    let published = app
        .messages_last_publish
        .expect("opening Settings publishes");
    let until = app
        .messages_words_change_in
        .expect("a record's age is said");
    assert!(until > TICK, "`3 h ago` moves at its next hour: {until:?}");
    assert_eq!(app.messages_publish_due_at(), Some(published + until));

    let (instant, wall) = (std::time::Instant::now(), std::time::SystemTime::now());
    app.count_sleep_since_last_turn(instant, wall);
    app.count_sleep_since_last_turn(instant, wall - NIGHT);
    assert_eq!(
        app.messages_publish_due_at(),
        Some(published + until),
        "CONTROL: a wall clock stepped back moves nothing"
    );
    // The lid closes for the night: the wall clock runs, the monotonic one
    // does not.
    app.count_sleep_since_last_turn(instant, wall + NIGHT);
    assert_eq!(
        app.messages_publish_due_at(),
        Some(published + TICK),
        "owed at once, a minute after the last publish at the soonest"
    );
    app.publish_native_messages_state_if_due(published + TICK);
    let republished = app.messages_last_publish.unwrap();
    assert_ne!(republished, published, "the slept page is republished");
    let until = app.messages_words_change_in.unwrap();
    assert!(until > TICK, "and owes its next hour again: {until:?}");

    // A zone that moved since the words were read: owed at once.
    let read_on = app.messages_words_offset_s;
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

/// THE DEVELOPER TOOLS LINE'S TWO CAPSULES, pressed on the page (owner ask
/// 2026-10-15): `Security` is a navigation — aterm's own Settings ▸ Security
/// opens and the line stays; the press that opens the Developer Tools pane
/// files the line ANSWERED (the owner did its one action) and writes NOTHING
/// of the Full Disk Access card's: no `opened` marker, no `opened_at` (a press
/// there is no answer to the file-access question). NEGATIVE CONTROL: the Full
/// Disk Access capsule's press writes both.
#[test]
fn the_developer_tools_line_opens_security_and_never_answers_the_file_access_card() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.except(&crate::message_reporters::devtools_entry(
        atpkg::devtools::Step::Add,
    ));
    let id = app.messages.digest().entries[0].id;
    assert_eq!(
        app.perform_message_act(wid, id.raw(), 1),
        MessageActOutcome::Performed {
            feedback: "Opened Settings \u{25b8} Security".to_string()
        }
    );
    assert_eq!(
        settings_view(&app, wid).map(|v| v.route),
        Some(SettingsRoute::Security)
    );
    assert!(
        app.messages.digest_entry(id).is_some(),
        "a navigation leaves the line"
    );
    // A headless arm opens no pane, and the page says so. The line is filed
    // answered either way: `act_digest` files the mover's press before it is
    // performed (the message center's existing rule, not this line's choice).
    assert_eq!(
        app.perform_message_act(wid, id.raw(), 0),
        MessageActOutcome::Refused {
            feedback: "System Settings did not open".to_string()
        }
    );
    assert!(app.messages.digest().is_empty(), "its one action: answered");
    assert_eq!(app.consent_card_host.marker, None, "no file-access marker");
    assert_eq!(app.consent_card_host.record.opened_at, None);
    // The control: the Full Disk Access capsule records the card's `opened`.
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

/// THE DEVELOPER TOOLS PRESS IS THE ONE HAND-OVER (owner ask 2026-10-15): on
/// a window the capsule opens the page through `App::hand_devtools_to_owner`,
/// shows the helper panel, notes the shared opened marker and has the owner's
/// return read the list, and never writes the file-access card's marker.
/// NEGATIVE CONTROL: a page that did not open notes nothing, shows nothing and
/// arms no return read. A windowed lane with stand-in arms stands in on a
/// headless App; nothing reaches the OS.
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

/// A DIGEST PRESS ON A LIST READ EMPTY (owner ask 2026-10-15) waits for `spctl
/// developer-mode enable-terminal` before the page can open, and the message
/// center files the line answered at the press (its one action). When the page
/// then does not open, the line comes back: the owner is not left with a press
/// that did nothing and a line put away for a month. NEGATIVE CONTROL: a page
/// that opens leaves the line filed. Stand-in arms on a headless App; nothing
/// reaches the OS.
#[cfg(target_os = "macos")]
#[test]
fn a_digest_press_whose_page_does_not_open_brings_its_line_back() {
    use crate::devtools_helper::test_arms;
    for (reach, back) in [(test_arms::refusing(), true), (test_arms::opening(), false)] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        test_arms::stand_in(&mut app, reach);
        app.devtools_helper.note_reading(
            test_arms::reading(
                aterm_containment::DevToolGrant::NotGranted,
                test_arms::absent(0),
            ),
            std::time::Instant::now(),
        );
        app.except(&crate::message_reporters::devtools_entry(
            atpkg::devtools::Step::Add,
        ));
        let id = app.messages.digest().entries[0].id;
        let _ = app.perform_message_act(wid, id.raw(), 0);
        assert!(
            app.messages.digest().is_empty(),
            "filed answered at the press"
        );
        assert!(
            app.devtools_helper.listing().is_some(),
            "the page waits for `spctl`"
        );
        let start = std::time::Instant::now();
        while app.devtools_helper.listing().is_some() {
            assert!(start.elapsed() < std::time::Duration::from_secs(60), "hang");
            app.tick_devtools_hand_over(std::time::Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            !app.messages.digest().is_empty(),
            back,
            "back exactly when the page did not open (back={back})"
        );
    }
}

/// `Open log` opens only an absolute regular file under the log dir: not a relative path,
/// a missing file, a file outside the dir, a `..` that starts under the dir and escapes
/// it, the dir itself, or a symlink in the dir (wherever it points); a refusal is a row
/// naming the path. The validator runs against a scratch dir standing in for the log dir;
/// the real one is asked only what it must refuse.
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

/// An instant past the grace of every row posted so far. Read AFTER the
/// post, never before it: `post` stamps the row with its own
/// `Instant::now()`, so a clock read ahead of the call falls inside the
/// grace whenever the thread is descheduled between the two for longer
/// than the margin (a loaded parallel run failed the heavy routine row's
/// fold test that way, 2026-09-28).
fn past_the_grace() -> Instant {
    Instant::now() + PROGRESS_GRACE + std::time::Duration::from_millis(10)
}

/// A classified `progress.json` read of a "net" pass started at `started`:
/// running mid-extract, or over (ended a minute later) — the tailer's reads
/// the silent-lane tests drive.
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

/// `appstatus` as the socket renders it.
fn appstatus(app: &App) -> Vec<String> {
    aterm_messages::wire::activity_rows_compat(
        &app.messages,
        Instant::now(),
        &crate::control::pct_encode,
    )
}

/// SILENT BY DEFAULT: A ROUTINE PASS RAISES NO ROW (upstream dbf97ecff's
/// `a_routine_pass_raises_no_row_and_records_every_outcome`, ported onto
/// the center; the rulings on the origin/main merge, 2026-09-23). Every row
/// the toolchain lane used to raise for a pass that updates what is already
/// installed — its announcement, its live meter, "installed", the
/// managed-current row, the machine-settings rows, a deferred pass —
/// re-gridded every window and resized every running TUI. Pinned for the
/// whole marker set of a routine pass: no row, no re-grid, no deadline —
/// and every outcome on `appstatus` in the bars' ledger grammar. The plan is
/// LIGHT: a routine pass of at least `HEAVY_PASS_BYTES` is very heavy
/// system use and raises its row (design §10.6, pinned by
/// `a_heavy_routine_pass_raises_one_row_after_the_grace_and_a_light_one_none`).
#[test]
fn a_routine_pass_raises_no_row_and_records_every_outcome() {
    let mut app = App::headless_for_test();
    let regrids = message_regrids();
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
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(message_regrids(), regrids, "no re-grid for routine work");
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

/// ONE FIRST-RUN CHILD, ONE OPEN AND ONE FOLD (upstream ac7942234's
/// `a_first_run_bar_holds_across_the_vendor_sub_pass_and_folds_at_exit`).
/// A first-run `atpkg update` runs the vendor lane before the ALab set: its
/// own `net-starting:`, meter, pass end and `net-installed: claude, codex`
/// come first, then the set's announcement and meter. Folding on the vendor
/// sub-pass's ending read or its marker reopened the row for the set — two
/// more re-grids of every window in the one pass meant to show exactly one
/// row. The row holds across it (a failed vendor sub-pass too) and folds
/// only at the child's exit.
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

/// A "net" read with no plan at all — atpkg begins its pass before the
/// signed index resolves, and the common outcome is a plan of nothing.
/// Running, or over (its writer gone, an end stamp written).
fn empty_read(running: bool) -> crate::PkgProgressSnapshot {
    let mut read = net_read(running, 1_700_000_000);
    read.file.overall = atpkg::progress::Overall::default();
    read.file.queue.clear();
    read.file.programs.clear();
    if !running {
        read.file.ended_unix = Some(1);
    }
    read
}

/// TC-5 — THE FIRST RUN'S ROW AT LAUNCH, NOT AT THE UPDATE'S ANNOUNCEMENT. The
/// lane opens it ahead of its seed, unsized; the seed's exit — a retried seed's
/// too — leaves it up; the update's announcement restates the SAME row with the
/// size, its reads paint it, and its exit is the one fold: one open, one fold,
/// whatever ran in between. An update that exits unclean without announcing
/// folds it as the Fault it was; one that exits clean without announcing
/// installed nothing, so the row is withdrawn with no Complete claimed.
#[test]
fn the_first_run_row_opens_ahead_of_the_seed_and_the_update_takes_it_over() {
    let regrids = message_regrids();
    let mut app = App::headless_for_test();
    assert!(app.open_first_run_ahead(), "a row goes up");
    let row = app
        .messages
        .live_by_key(toolchain_words::KEY_PASS)
        .map(|l| (l.id, l.msg.title.clone()))
        .expect("the row opens at once");
    assert_eq!(row.1, "Installing ALab tools");
    let row = row.0;
    app.toolchain_pass_ended(true);
    assert!(
        app.messages.live(row).is_some(),
        "the seed's exit carries it"
    );
    app.announce_toolchain_pass(
        "installing 10 ALab program(s) over the network (about 3 GB on disk when finished)",
        true,
    );
    let live: Vec<_> = app.messages.live_rows().map(|l| l.id).collect();
    assert_eq!(live, vec![row], "the announcement restates it in place");
    let stats = app
        .messages
        .live(row)
        .and_then(|l| l.msg.meter.as_ref().map(|m| m.stats.clone()));
    assert_eq!(stats.as_deref(), Some("10 programs \u{00b7} ~3 GB"));
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    assert_eq!(
        app.messages.live_rows().map(|l| l.id).collect::<Vec<_>>(),
        vec![row]
    );
    app.toolchain_pass_ended(true);
    assert_eq!(
        app.messages.echoes().first().map(|e| e.kind),
        Some(EchoKind::Complete),
        "the announced update's clean exit is its Complete"
    );
    app.settle_messages(Instant::now() + quiet_past_echoes());
    assert_eq!(
        app.messages.live_rows().count(),
        0,
        "the update's exit folds it"
    );
    assert_eq!(message_regrids(), regrids + 2, "one open, one fold");

    // A retried seed is carried too; an unclean update with nothing announced folds it.
    let regrids = message_regrids();
    let mut app = App::headless_for_test();
    assert!(app.open_first_run_ahead());
    app.toolchain_pass_ended(true);
    assert!(
        !app.open_first_run_ahead(),
        "a retried seed only carries the row it already holds"
    );
    app.toolchain_pass_ended(true);
    assert_eq!(app.messages.live_rows().count(), 1, "still the one row");
    app.toolchain_pass_ended(false);
    assert_eq!(
        app.messages.echoes().first().map(|e| e.kind),
        Some(EchoKind::Fault),
        "it ends as the update exited"
    );
    app.settle_messages(Instant::now() + quiet_past_echoes());
    assert_eq!(app.messages.live_rows().count(), 0);
    assert_eq!(message_regrids(), regrids + 2, "one open, one fold");

    // A clean update that never announced installed nothing over the wire (an
    // architecture no build serves): no Complete for an install that did not happen.
    let mut app = App::headless_for_test();
    assert!(app.open_first_run_ahead());
    let row = app
        .messages
        .live_by_key(toolchain_words::KEY_PASS)
        .map(|l| l.id)
        .expect("the row");
    app.toolchain_pass_ended(true);
    app.toolchain_pass_ended(true);
    assert!(app.messages.live(row).is_none(), "the row is gone");
    assert!(
        app.messages
            .echoes()
            .iter()
            .all(|e| e.kind != EchoKind::Complete),
        "{:?}",
        app.messages.echoes()
    );
    assert_eq!(
        app.messages.log().get(row).unwrap().state,
        LogState::Retired(Retired::Withdrawn)
    );
}

/// …and where no update follows the seed (the lane parks, or Automatic updates
/// went off), the lane withdraws the row: no outcome claimed, nothing left on
/// glass, and a later child's exit has nothing of it to fold. A withdrawal after
/// an announcement took the row over leaves that child its row.
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

/// THE FIRST-RUN ANNOUNCEMENT OPENS THE ROW BEFORE ANY SNAPSHOT (upstream
/// dbf97ecff's `a_first_run_announcement_opens_the_toolchain_bar_before_any_snapshot`,
/// ported onto the App; the row's own words are `toolchain_words`'). The row
/// goes up at the marker — before `progress.json` exists — live under the
/// announcement's cap rather than a fold. A pass that plans nothing
/// never opens a row of its own. The announced row is LATCHED (design
/// §10.5 H8): neither an ended-empty read, its own vanished read nor the
/// lane's untagged clear at the waited child's exit folds it; the child's
/// exit WITHDRAWS it — the log keeps the row, and `appstatus` claims no
/// `ok` for a pass whose marker never came (ruling 13).
#[test]
fn a_first_run_announcement_opens_the_row_before_any_snapshot() {
    let mut app = App::headless_for_test();
    app.announce_toolchain_pass(
        "installing 10 ALab program(s) from the bundled registry (about 3 GB on disk when finished)",
        true,
    );
    assert_eq!(app.messages.live_rows().count(), 1);
    assert_eq!(app.message_band_rows, 1, "on glass at once");
    {
        let live = app
            .messages
            .live_by_key(toolchain_words::KEY_PASS)
            .expect("the announcement opens the row");
        assert_eq!(live.fold_at, None, "live: no fold…");
        assert!(live.stale_at.is_some(), "…but a cap");
    }
    assert!(app.messages_deadline().is_some(), "the cap arms a wake");
    // A pass that plans nothing never opens a row of its own…
    let mut quiet = App::headless_for_test();
    quiet.apply_toolchain_snapshot(Some(&empty_read(true)), true);
    assert_eq!(
        quiet.messages.live_rows().count(),
        0,
        "unplanned running pass: no row"
    );
    assert_eq!(quiet.message_band_rows, 0);
    // …and an announced one that ends empty folds at its child's exit.
    let row = app
        .messages
        .live_by_key(toolchain_words::KEY_PASS)
        .map(|l| l.id)
        .expect("the first-run row");
    app.apply_toolchain_snapshot(Some(&empty_read(true)), true);
    app.apply_toolchain_snapshot(Some(&empty_read(false)), true);
    app.apply_toolchain_snapshot(None, true);
    app.apply_toolchain_snapshot(None, false);
    assert!(
        app.messages.live(row).is_some(),
        "latched: the first run's child still runs, and its exit folds it"
    );
    app.toolchain_pass_ended(true);
    assert!(app.messages.live(row).is_none(), "folded at exit");
    assert_eq!(
        app.messages.log().get(row).unwrap().state,
        LogState::Retired(Retired::Withdrawn)
    );
    assert!(
        appstatus(&app).is_empty(),
        "and it claims no outcome: {:?}",
        appstatus(&app)
    );
}

/// An ANSWER folds a live row no child is latched behind (one carried across
/// a handoff), its record on `appstatus` (upstream `toolchain_ended` →
/// `retire_answered_toolchain`; a latched first run's answer waits for the
/// exit: `a_first_run_row_holds_across_the_vendor_sub_pass_and_folds_at_exit`).
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
/// A parked outgoing handoff (the parent's half of the freeze predicate) with nothing live
/// behind it, like the handoff tests' `parked_record`.
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

/// The manifest's window carry around what `carried_messages` handed over,
/// as the two construction sites in `app_update_handoff` write it.
pub(super) fn window_carry(
    rows: u16,
    carried: crate::session_store::MessagesCarry,
) -> crate::session_store::WindowCarry {
    crate::session_store::WindowCarry {
        rows: 24,
        cols: 80,
        outer_x: None,
        outer_y: None,
        inner_px: None,
        presence_row: false,
        status_bar_rows: rows,
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

#[cfg(unix)]
/// In production order (the manifest captures the carry, then the attempt parks): a row
/// posted under the park drains to the writer at once while its hold waits, the flush the
/// parent runs before it execs lands it on disk, and the carry ships rows, never lines.
/// The successor's drain waits until Commit, so a refused successor writes nothing.
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
    // The capture, then the park: the order the manifest imposes.
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
    // The exec: the flush lands everything before the process is
    // replaced, and leaves the writer open for a failed exec.
    app.flush_messages_log();
    assert!(app.messages_log.is_some());
    let text = std::fs::read_to_string(&path).expect("the log was written");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains("before the park"), "{}", lines[0]);
    assert!(lines[1].contains("during the park"), "{}", lines[1]);
    drop(app.messages_log.take());

    // The successor before Commit: its lines wait; Commit drains them.
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
/// THE HANDOFF CARRY, the host's half (design §3.8; the carry's own encoding,
/// glass order, holds and caps are the engine's `center` carry tests, a
/// carried check warning's fate `a_carried_check_warning_heals_by_the_successors_first_healthy_check`):
/// the update row moves on to "finishing" only when this process is the
/// handed-off successor AND the carry had one — the successor adopts it as its
/// flow — and nothing is invented without one; nothing crosses as a log
/// line; the geometry is the parent's until Commit (ids continuing above the
/// parked parent's band are
/// `a_record_the_parked_parent_makes_never_shares_an_id_with_the_successor`'s);
/// an older parent's `bars` still decode and seed nothing; an empty carry
/// raises nothing.
#[test]
fn a_handoff_carries_live_rows() {
    let mut parent = App::headless_for_test();
    let installing = parent.post_message(update_words::installing("0.76.0"));
    parent.post_message(toolchain_words::announced("installing ay, trust"));
    parent.pending_update_handoff = Some(parked());
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());

    // The successor, before Commit.
    let mut successor = App::headless_for_test();
    successor.incoming_handoff_pending = true;
    successor.message_band_rows = carry.status_bar_rows.min(MAX_ROWS);
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
    assert_eq!(
        successor.message_band_rows, carry.status_bar_rows,
        "the geometry is the parent's until Commit"
    );

    // Not the successor of a handoff: the words come back as they were.
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

    // No progress row carried: nothing is invented.
    let mut other = App::headless_for_test();
    other.post_message(toolchain_words::announced("installing ay"));
    let no_progress = window_carry(1, other.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&no_progress, Some("0.76.0"));
    assert!(
        successor
            .messages
            .live_by_key(update_words::KEY_PROGRESS)
            .is_none()
    );

    // An older parent's `bars` (the status bars' carry): the manifest still decodes
    // (serde skips the key) and seeds nothing; the successor's own reporters say those
    // lanes at Commit.
    let old: crate::session_store::WindowCarry = aterm_toml::from_str(
        "rows = 24\ncols = 80\nstatus_bar_rows = 1\n\n[[bars]]\nlane = \"toolchain\"\n\
         glyph = \"!\"\ntitle = \"ALab toolchain\"\ndetail = \"\"\ntone = \"warn\"\n\
         fill_permille = 430\n",
    )
    .expect("an older parent's window carry still decodes");
    assert_eq!(
        old.status_bar_rows, 1,
        "the rows it had committed still count"
    );
    let mut adopting = App::headless_for_test();
    adopting.seed_carried_messages(&old, Some("0.76.0"));
    assert_eq!(
        adopting.messages.live_rows().count(),
        0,
        "no bar is carried"
    );
    // An empty carry seeds nothing and raises nothing.
    let mut empty = App::headless_for_test();
    empty.seed_carried_messages(&window_carry(0, Default::default()), Some("0.76.0"));
    assert_eq!(empty.messages.live_rows().count(), 0);
    assert_eq!(empty.messages.log().next_id(), MessageId::FIRST);
}

#[cfg(unix)]
/// The parked parent's id band is reserved before the seed: when the successor's boot
/// ring holds another writer's record under a carried row's number, the seed moves that
/// record to a fresh id above the band the parked parent keeps minting in
/// (`PARKED_PARENT_ID_RESERVE`), never the parent's own next id.
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
    successor.messages = aterm_messages::MessageCenter::new(boot, Instant::now());
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(
        &window_carry(parent.message_band_rows, carried.clone()),
        Some("0.76.0"),
    );
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
/// The landing answers the apply lane's last outcome: "Update waits for you" and
/// "Couldn't install aterm vX · Install now", carried on glass across the handoff that
/// installed the build, come down at the successor's landing rather than re-arming a
/// fresh hold beside "Updated to aterm vX".
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
        let carry = window_carry(parent.message_band_rows, parent.carried_messages());
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

/// Every window of `app` shows its band on screen (`band_on_screen_for_test`):
/// a row said there is seen, so it holds its time and folds, and nothing is owed
/// (U37: a row said where no band is seen waits to be seen).
fn band_seen(app: &mut App) {
    for ws in app.windows.values_mut() {
        ws.band_on_screen_for_test = true;
        ws.occluded = false;
    }
}

/// The successor of a parent that raised `raise`, carried on the explicit
/// lane (an installing row crosses) or the automatic one (none does),
/// committed and landed: the realistic state its next event meets.
#[cfg(unix)]
fn landed_successor(automatic: bool, raise: impl FnOnce(&mut App)) -> App {
    let mut parent = App::headless_for_test();
    raise(&mut parent);
    if !automatic {
        parent.post_message(update_words::installing("9.9.9"));
    }
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.messages.after_handoff_commit(Instant::now());
    successor.post_update_landed("9.9.9", 7, 0);
    successor
}

/// The details of every "aterm updates work again" on `app`'s record.
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

/// One beat of the successor's own checker (its first generation), as the
/// watchdog reads it.
#[cfg(unix)]
fn successor_beat(checks: u64, deferrals: u64) -> aterm_update::checker_watch::CheckerBeat {
    aterm_update::checker_watch::CheckerBeat {
        generation: 1,
        phase: aterm_update::checker_watch::CheckerPhase::Waiting,
        at_secs: 100,
        max_interval_secs: 1_100,
        respawns: 0,
        deferrals,
        settings_misses: 0,
        lane: None,
        checks,
    }
}

#[cfg(unix)]
/// A carried stall that never healed (the parent's checker stalled holding the check lane,
/// its row folded, then an automatic landing) is the successor's watchdog's to heal, never
/// a latch: the successor's own later stall is said with its row, and its first completed
/// check heals it. Control: a successor that carried nothing says its stall the same way.
#[test]
fn a_carried_unhealed_stall_never_silences_the_successors_own_watchdog() {
    use crate::update_checker_watch::CheckerStall;
    use aterm_update::checker_watch::{CheckerBeat, CheckerPhase};
    let stalled = update_words::HealthKind::Stalled;
    for carried in [true, false] {
        let mut parent = App::headless_for_test();
        band_seen(&mut parent);
        if carried {
            // Stalled inside its lane: the replacement cannot check, so the
            // warning is never healed.
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
        // The automatic lane: no progress row crosses.
        let carry = window_carry(parent.message_band_rows, parent.carried_messages());
        assert_eq!(carry.update_health_said.len(), usize::from(carried));
        let mut successor = App::headless_for_test();
        successor.seed_carried_messages(&carry, Some("9.9.9"));
        successor.messages.after_handoff_commit(Instant::now());
        successor.post_update_landed("9.9.9", 7, 0);
        // The successor's own fresh checker stalls later.
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
            "the successor's own stall reaches the band (carried: {carried})"
        );
        let beat = CheckerBeat {
            generation: 2,
            phase: CheckerPhase::Waiting,
            at_secs: 100,
            max_interval_secs: 1_100,
            respawns: 1,
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
/// ROUND-EIGHT UPDATE AUDIT, FINDINGS 15 (the check case) AND 301 (scenario
/// A), WITH 302's REALISTIC NEXT EVENT — A CARRIED CHECK WARNING IS HEALED
/// BY THE SUCCESSOR'S FIRST HEALTHY CHECK, AND NEVER MUTES ITS NEXT EPISODE.
/// The parent raised "Couldn't check for updates"; a build landed (which
/// answers only an install warning, finding 54). The successor's first
/// check came back up to date, which clears the streak in the ledger — and
/// its updater, which had announced nothing, sent nothing: the latch stood
/// for the launch, and hours later a new failing episode was a log line, no
/// row. The class is now handed to this process's updater as announced
/// (`aterm_update::adopt_announced_health`; its own tests prove the
/// recovery an up-to-date check then sends), whose recovery is the proof:
/// the warning lifts and the healing is on record against the parent's words
/// (the next episode speaking is `carried_health_conformance`'s `Announce`).
/// Both lanes — the automatic one carries no progress row, only what the
/// parent said.
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
        // Still failing: the adopting updater restates it, quietly — and the
        // landing, which proves nothing about checking (finding 54), left the
        // carried row standing, never a second beside it.
        assert!(!successor.note_update_health(aterm_update::HEALTH_RESTATED_TITLE, body));
        assert!(healings(&successor).is_empty());
        assert_eq!(
            successor.live_update_health(|k| k == check).len(),
            1,
            "the carried check warning stands, alone (automatic: {automatic})"
        );
        // Up to date: the adopting updater's recovery, naming the class.
        assert!(!successor.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, "manifest"));
        assert_eq!(
            healings(&successor),
            vec!["after \"Couldn't check for updates\", since Sep 14".to_string()],
            "the healing is on record against what the parent said (automatic: {automatic})"
        );
        assert!(successor.live_update_health(|k| k == check).is_empty());
        assert!(!owes_entry(&successor, check), "and its digest line");
    }
    // FOLDED in the parent before the park (host-msgs-R44-HOST-1): no
    // carried row names it, but what the parent said does — still handed
    // over, once.
    UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().clear());
    let _folded = landed_successor(false, |parent| {
        band_seen(parent);
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
    // NEGATIVE CONTROL: a successor that carried nothing hands nothing over.
    UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().clear());
    let _quiet = landed_successor(true, |_| {});
    assert!(UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow().is_empty()));
}

#[cfg(unix)]
/// A carried warning's "To look at" line says what its parent's would have (when and
/// why), on both seamless lanes and the cold lane. Control: a parent older than the
/// sentence's carry hands the excerpt alone.
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
/// ROUND-EIGHT UPDATE AUDIT, FINDINGS 15 AND 301 (lock-held), WITH 302's
/// REALISTIC NEXT EVENT — A CARRIED LOCK-HELD WARNING IS THE SUCCESSOR'S
/// WATCHDOG'S TO HEAL. A landing proves nothing about a sibling aterm holding
/// the machine's check lock, but the successor's own checker sees the
/// streak's end: its first check completed without deferring (as for a
/// replacement generation, round six's finding 9). Latched with nothing to
/// heal it, the next lock-held episode was swallowed. The same holds for a
/// carried STALL ("Update check stopped", about the parent's check thread:
/// the successor runs a new one and heals it by the same proof, finding 301
/// scenario B). NEGATIVE CONTROLS: no checker yet, or one that has checked
/// nothing, heals nothing; and a sibling still stuck is taken over by the
/// successor's own deferrals — said once, not twice, healed when it ends.
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
        // The sibling let go before the successor's first check.
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
        // A later streak is said again.
        successor.look_at_update_checker(Some(successor_beat(2, CHECKER_UNGATED_AFTER)), 300);
        assert_eq!(
            rows(&successor),
            1,
            "a new streak speaks (automatic: {automatic})"
        );

        // The sibling is still stuck: the successor's deferrals take the
        // streak over — no second row — and its end heals it.
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
/// Only a warning the ledger raised is the successor's updater's to heal: the strand
/// notice reads as the check kind but is no ledger failure, so it is only remembered (an
/// adopted class would heal it over a machine still stranded) and the successor says its
/// own strand in one row. Controls: an older build's words for a ledger class are adopted;
/// a title this build does not know is not.
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
        // The successor's own strand notice, on its first stranded cycle.
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
    // Controls: the words decide, not the kind they are read as.
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
/// The cold lane carries what the parent's warnings said across its `exec`
/// (`bind_cold_lane_health_said`), so the successor's landing records the healing of the
/// parent's install warning. Control: nothing carried, or a value that does not parse,
/// records nothing.
#[test]
fn a_cold_lane_landing_records_the_healing_of_the_parents_install_warning() {
    let body = "3 failed attempts to install in a row since 2026-09-24T01:00:00Z: why.";
    let mut parent = App::headless_for_test();
    assert!(parent.note_update_health(aterm_update::health_failing_title("apply"), body));
    // std's Command on purpose: never spawned, it is the shipped cold-lane
    // exec's type, which `bind_cold_lane_health_said` takes.
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
        // The successor, as the window's entry builds it on a cold landing.
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

/// The Commit flush in the park tests: their subject is what the successor reads once the
/// parent's lines have landed, not how fast they land, so the wait is a hang detector, not
/// the production `MESSAGES_FLUSH_COMMIT_WAIT` (250 ms), which a starved writer can miss
/// under load.
#[cfg(unix)]
const COMMIT_FLUSH_IN_TESTS: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

#[cfg(unix)]
/// The successor re-reads the file after Commit, so a row the parent resolved after the
/// successor's boot is Resolved in its history (never Stale) and a record written after
/// the boot is there.
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
    // The successor boots: its ring is the file as it is now.
    let mut successor = App::headless_for_test();
    successor.messages = aterm_messages::MessageCenter::new(
        crate::messages_store::load_tail(&path, u64::MAX).log,
        Instant::now(),
    );
    assert_eq!(
        successor.messages.log().get(x).and_then(LogRecord::retired),
        Some(&Retired::Stale),
        "the boot reads the parent's open row as nobody's"
    );
    successor.incoming_handoff_pending = true;
    // The parent carries on: it resolves X and records R, then parks.
    parent.resolve_message(x, Outcome::Ok);
    let r = parent.record_message(warn("written after the boot"));
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    parent.pending_update_handoff = Some(parked());
    // Commit: the parent's last write, then its `_exit` (the flush waits
    // as a hang detector: see `COMMIT_FLUSH_IN_TESTS`).
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
/// A carried row the parked parent ended after the capture stays ended in the successor,
/// with no second log line, and a warning it healed is no longer this process's to heal.
/// Control: a row nobody ended stays live.
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
        // The parent's loop runs on: a heal, a reporter's resolve.
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
/// A warning the parked parent dismissed stays dismissed in the successor, but a
/// dismissal records no healing: what it said and its day's entry stay, and the proof
/// here records its healing once.
#[test]
fn a_health_warning_the_parked_parent_dismissed_is_still_healed_on_record() {
    let (mut successor, dir) = committed_after_park(
        "parked-dismissal",
        say_check_warning,
        // The person presses ✕ on the warning while the parent is parked.
        |parent| {
            let warning = parent.live_update_health(|_| true);
            assert_eq!(warning.len(), 1, "PRECONDITION: the warning is up");
            assert!(parent.messages.dismiss(warning[0], Instant::now()));
        },
    );
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

/// A seamless parent writing `messages.log` in a scratch folder named for `name`: it does
/// `raise`, parks (capturing its carry), does `while_parked`, flushes and exits; its
/// successor seeds the carry and commits, re-reading the file. Returns the successor
/// (writer open) and the folder.
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
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
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

/// The parent's check warning, said through the one door.
#[cfg(unix)]
fn say_check_warning(app: &mut App) {
    assert!(app.note_update_health_as(
        update_words::HealthKind::Check,
        "Couldn't check for updates",
        "3 failed checks in a row since 2026-09-28T00:00:00Z: x",
    ));
}

/// Every "aterm updates work again" on `app`'s record.
#[cfg(unix)]
fn healing_count(app: &App) -> usize {
    app.messages
        .log()
        .records()
        .filter(|rec| rec.title == "aterm updates work again")
        .count()
}

#[cfg(unix)]
/// A healing the parked parent recorded is on record once, whatever its row did
/// (dismissed in the park, or folded before it): the successor reads the healing in the
/// file and drops what the warning said, its latch and its day's entry. A download
/// warning the parent did not heal stays for the proof here.
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
                band_seen(parent);
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
                // The heal, in the park: no live row to resolve, and the
                // healing goes on record all the same.
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
        // Control: the kind the parent's check did not answer is still this process's.
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
/// Control: only a healing the parked parent wrote after the capture (at or above the
/// carry's next id) answers a carried warning; an earlier episode's healing in the same
/// words does not.
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
/// A carried warning of a kind the parked parent healed ends with its memory, even when
/// the file gives no ending for its row (`end_carried_rows_the_parent_ended` ends rows
/// only by the four verdicts on the row).
#[test]
fn a_kind_the_parked_parent_healed_leaves_no_row_whatever_the_file_says_of_it() {
    let mut parent = App::headless_for_test();
    say_check_warning(&mut parent);
    parent.sync_messages();
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    let from = carry.next_message_id;
    // The parent's records after the capture: in its band, stamped since.
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
/// A healed warning's row ends though another window took its number first: the file
/// loads the parent's record under a fresh id, so the row's ending is not read by the
/// number alone.
#[test]
fn a_healed_warning_s_row_ends_though_another_writer_took_its_number() {
    let name = "parked-collided-heal";
    let path = std::env::temp_dir()
        .join(format!("aterm-messages-{name}-{}", std::process::id()))
        .join(crate::messages_store::FILE_NAME);
    let (successor, dir) = committed_after_park(
        name,
        |parent| {
            // The other window writes under the number the warning takes.
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
/// Another window's healing in the same words is not the parked parent's: every aterm
/// appends to one `messages.log` with its own id counter, so W2's healing is told apart by
/// the parent's id band (W2 far above it) or by the capture's stamp (W2 on the parent's
/// next number), and the parent's still-failing warning stays this process's to heal.
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
        // W2, a window that shares the file, says and heals the warning.
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
            // The capture's stamp is after it, by the clock.
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
/// No id is handed out twice across the park: the parked parent records on past the
/// carry's next id while the successor records before and at Commit; a `since=` reader
/// past the parent's last id sees every successor record, and messages.log names each
/// id once.
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
    // The successor boots on the file as it is.
    let mut successor = App::headless_for_test();
    successor.messages = aterm_messages::MessageCenter::new(
        crate::messages_store::load_tail(&path, u64::MAX).log,
        Instant::now(),
    );
    successor.incoming_handoff_pending = true;
    // The park: the carry's next id is taken now…
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    parent.pending_update_handoff = Some(parked());
    // …and the parked parent records on, as `appnotice` and a harness do.
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
    // The successor, before Commit, records one of its own.
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    let own = successor.record_message(warn("the successor's own").hold(Hold::LogOnly));
    // Commit: the parent's last write, then its `_exit`.
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
/// A NOTICE WHILE PARKED IS TOLD TO RETRY (round-seven update audit,
/// finding 32): the carry was captured at the park, so a post the parked
/// parent accepted was never shown by the successor and its id was handed
/// out again. Now the parent answers the transient `ERR busy` class and
/// mints nothing; unparked, the same post is taken.
#[test]
fn a_notice_while_parked_is_refused_for_a_retry_and_mints_nothing() {
    let mut parent = App::headless_for_test();
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
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
    // The successor's ids start where the carry said, and no script holds one.
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    let next = successor.post_message(warn("after commit"));
    assert!(next >= before);
    // A stand-down unparks, and the retry is taken.
    parent.pending_update_handoff = None;
    let reply = parent.take_notice(req());
    assert!(reply.starts_with("OK message="), "{reply}");
}

#[cfg(unix)]
/// A row about the old process leaves with it: "Screen reader access lost · restart aterm
/// to retry" is answered by the successor, a new process with its own publisher, so a
/// carried one never stands over access that works.
#[test]
fn a_carried_dead_publisher_row_leaves_with_the_process_it_described() {
    let mut parent = App::headless_for_test();
    parent.post_message(crate::message_reporters::a11y_publisher_dead(
        "bus gone", true,
    ));
    parent.post_message(warn("a config warning"));
    // A standing row is born late (the Row Law): the parent's grace ends
    // before the carry, as it does in a real update.
    let _ = parent
        .settle_messages(Instant::now() + PROGRESS_GRACE + std::time::Duration::from_millis(10));
    let carried = parent.carried_messages();
    assert_eq!(carried.messages.len(), 2, "both rows are carried");
    let carry = window_carry(parent.message_band_rows, carried);
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
/// An intent only the old process could perform does not cross: the keeper's recovery row
/// carried into a successor that reattached no shells itself (`keeper_recovered` empty)
/// keeps its words but not `End sessions`, which would end nothing; a successor that did
/// reattach keeps the press.
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
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
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

#[cfg(unix)]
/// THE QUEUED CARRY converges at Commit (design §3.8; the carried rows' own
/// lifetimes after Commit are the engine's `carry_round_trips_and_folds_after_commit`):
/// the one state in which the two counts disagree — a row the parent's frozen
/// count had queued — converges on the center's count at the first unfrozen
/// sync.
#[test]
fn a_queued_carried_row_converges_on_the_centers_count_at_commit() {
    let now = Instant::now();
    // THE QUEUED CARRY. A one-row band, a second row of the same rank
    // posted under the parent's freeze: it crosses queued; the successor's
    // center commits two rows for it while the geometry keeps the
    // parent's one — until Commit, when the geometry takes the center's
    // count even though `commit_rows` itself reports no move.
    let mut parent = App::headless_for_test();
    let first =
        parent.post_message(Message::new(tags::SESSION, Severity::Success, "first").line("one"));
    assert_eq!(parent.message_band_rows, 1);
    parent.pending_update_handoff = Some(parked());
    let second =
        parent.post_message(Message::new(tags::SESSION, Severity::Success, "second").line("two"));
    assert!(
        parent.messages.live(second).unwrap().is_queued(),
        "the frozen count has one slot and the earlier row holds it"
    );
    let carried = parent.carried_messages();
    assert_eq!(carried.messages.len(), 2);
    assert!(carried.messages[0].on_glass && !carried.messages[1].on_glass);
    let carry = window_carry(parent.message_band_rows, carried);
    let mut successor = App::headless_for_test();
    successor.incoming_handoff_pending = true;
    successor.message_band_rows = carry.status_bar_rows.min(MAX_ROWS);
    successor.seed_carried_messages(&carry, None);
    assert_eq!(successor.messages.committed_rows(), 2);
    assert_eq!(successor.message_band_rows, 1);
    assert!(!successor.sync_message_band_rows(now), "frozen");
    successor.incoming_handoff_pending = false;
    successor.messages.after_handoff_commit(now);
    assert_eq!(
        successor.messages.commit_rows(now, MAX_ROWS),
        None,
        "the center's count already stands at two…"
    );
    assert!(
        successor.sync_message_band_rows(now),
        "…so the geometry converges on it here"
    );
    assert_eq!(successor.message_band_rows, 2);
    assert!(successor.messages.live(first).is_some() && successor.messages.live(second).is_some());
    assert!(!successor.messages.live(second).unwrap().is_queued());
}

// ---- Attention and motion (design §10.5, §10.8, §10.12) --------------

/// A light routine read: [`net_read`]'s pass with a plan far under
/// [`toolchain_words::HEAVY_PASS_BYTES`], the pass that paints nothing.
fn light_read(running: bool, started: u64) -> crate::PkgProgressSnapshot {
    let mut snap = net_read(running, started);
    snap.file.overall.bytes_done = 12_000_000;
    snap.file.overall.bytes_total = 40_000_000;
    snap
}

/// The newest record under `key` that retired as a record.
fn recorded(app: &App, key: &str) -> Option<LogRecord> {
    app.messages
        .log()
        .records()
        .rev()
        .find(|r| r.key.as_deref() == Some(key))
        .filter(|r| r.retired() == Some(&Retired::Recorded))
        .cloned()
}

/// WHO STARTED IT DECIDES THE ROW (design ruling 220): the same slow
/// download costs NO re-grid when the background loop runs it, and two —
/// its row opens once, its end (the staged record) resolves it into a
/// Complete echo that holds the slot, and the row is given back once —
/// when a person started it (Check for Updates, or `aterm ctl update
/// check` while it runs).
#[test]
fn an_automatic_update_costs_no_regrid_and_a_persons_at_most_two() {
    for asked in [false, true] {
        let mut app = App::headless_for_test();
        if asked {
            app.note_update_check_asked(true);
        }
        let regrids = message_regrids();
        app.note_update_progress(&aterm_update::Progress::Downloading {
            version: "9.9.9".into(),
            bytes_done: 10_000_000,
            bytes_total: 74_000_000,
        });
        let t = past_the_grace();
        app.settle_messages(t);
        assert_eq!(app.message_band_rows, u16::from(asked), "asked={asked}");
        assert_eq!(message_regrids(), regrids + u64::from(asked));
        if asked {
            // The check ends while the row is up: it keeps moving to its
            // end, so its echo and the record stay one story.
            app.note_update_check_asked(false);
            app.note_update_progress(&aterm_update::Progress::Downloading {
                version: "9.9.9".into(),
                bytes_done: 60_000_000,
                bytes_total: 74_000_000,
            });
            assert_eq!(app.messages.live_rows().count(), 1, "the row keeps moving");
        }
        app.note_update_progress(&aterm_update::Progress::Staged {
            version: "9.9.9".into(),
            build: 41,
        });
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(
            app.messages.echoes().first().map(|e| e.kind),
            asked.then_some(EchoKind::Complete),
            "asked={asked}: a person's download ends in its Complete echo"
        );
        let mut t = t;
        for _ in 0..40 {
            t += std::time::Duration::from_millis(100);
            app.settle_messages(t);
        }
        assert_eq!(app.message_band_rows, 0, "asked={asked}");
        assert_eq!(
            message_regrids(),
            regrids + 2 * u64::from(asked),
            "asked={asked}: one open, one fold — or nothing"
        );
        let rec = recorded(&app, update_words::KEY_PROGRESS).expect("the staged record");
        assert!(rec.title.contains("9.9.9"), "asked={asked}: {}", rec.title);
    }
}

/// A download nobody is waiting on that fails is a record in the flow's words, with the
/// version its report did not carry kept from the quiet download: no row came, so no
/// echo goes.
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
    assert!(app.messages.echoes().is_empty());
    assert!(app.update_quiet.is_none());
    assert!(
        app.messages
            .log()
            .records()
            .any(|r| r.severity == Severity::Warn && r.title.contains("9.9.9")),
        "the failure's record names the version"
    );
}

/// A rate limit (`Progress::Deferred`) or a release withheld mid-download is no failure:
/// the revealed download row fades (no `⚠ failed`) and the deferral is a record. A real
/// failure still ends in its Fault echo.
#[test]
fn a_deferred_download_fades_and_a_failed_one_faults() {
    for (progress, kind) in [
        (
            aterm_update::Progress::Deferred {
                detail: "rate limited".into(),
            },
            EchoKind::Vanish,
        ),
        (
            aterm_update::Progress::Withheld {
                detail: "held: build 7 was signed by machine m11, which the machine \
                         roster has revoked"
                    .into(),
            },
            EchoKind::Vanish,
        ),
        (
            aterm_update::Progress::Failed {
                detail: "could not verify the download".into(),
            },
            EchoKind::Fault,
        ),
    ] {
        let mut app = App::headless_for_test();
        // A person's check: its download has the row.
        app.note_update_check_asked(true);
        app.note_update_progress(&aterm_update::Progress::Downloading {
            version: "9.9.9".into(),
            bytes_done: 10_000_000,
            bytes_total: 74_000_000,
        });
        app.settle_messages(past_the_grace());
        assert_eq!(app.message_band_rows, 1, "revealed");
        app.note_update_progress(&progress);
        assert_eq!(app.messages.live_rows().count(), 0, "{progress:?}");
        assert_eq!(
            app.messages.echoes().first().map(|e| e.kind),
            Some(kind),
            "{progress:?}"
        );
    }
}

/// The landing resolves the live Finishing row `Ok` (its Complete echo plays), and the
/// good news is a record.
#[test]
fn the_landing_resolves_the_finishing_row_and_records_the_words() {
    let mut app = App::headless_for_test();
    let finishing = app.post_message(update_words::finishing("0.91.0"));
    app.settle_messages(Instant::now() + PROGRESS_GRACE * 2);
    assert!(
        app.messages.glass_position(finishing).is_some(),
        "on the glass"
    );
    app.post_update_landed("0.91.0", 41, 0);
    assert!(app.messages.live(finishing).is_none());
    assert!(matches!(
        app.messages
            .log()
            .get(finishing)
            .and_then(LogRecord::retired),
        Some(Retired::Resolved(Outcome::Ok))
    ));
    assert_eq!(
        app.messages.echoes().first().map(|e| e.kind),
        Some(EchoKind::Complete)
    );
    assert_eq!(
        app.messages.live_rows().count(),
        0,
        "the landing takes no row"
    );
    let landed = app
        .messages
        .log()
        .records()
        .rev()
        .find(|r| r.tag == aterm_messages::tags::UPDATE)
        .expect("the landing's record");
    assert_eq!(landed.retired(), Some(&Retired::Recorded));
}

/// A navigation press that took the person to fix the thing read the row: a held row
/// folds; a live row stays.
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

/// A refused install: the Installing row an explicit apply added resolves `Warn` (a
/// refusal is not a delivery) and its indicator ends in the Fault echo.
#[test]
fn a_refused_install_resolves_warn_and_echoes_fault() {
    let mut app = App::headless_for_test();
    app.begin_update_installing(true);
    let id = app
        .messages
        .live_by_key(update_words::KEY_PROGRESS)
        .expect("the explicit apply's row")
        .id;
    assert!(
        app.messages.glass_position(id).is_some(),
        "committed at once"
    );
    app.retire_update_installing();
    assert!(app.messages.live(id).is_none());
    assert!(matches!(
        app.messages.log().get(id).and_then(LogRecord::retired),
        Some(Retired::Resolved(Outcome::Warn))
    ));
    assert_eq!(
        app.messages.echoes().first().map(|e| e.kind),
        Some(EchoKind::Fault)
    );
}

/// VERY HEAVY SYSTEM USE (design §10.6, H8): a routine pass that planned
/// at least `HEAVY_PASS_BYTES` raises its one row — after the grace, so
/// the glass moves only if the work lasts — and a light one raises none.
/// ONE OPEN, ONE FOLD across the child's sub-passes (§10.2 #6): the row is
/// LATCHED — a sub-pass's answer and its ending read fold nothing — and
/// the child's exit folds it with its Complete echo.
#[test]
fn a_heavy_routine_pass_raises_one_row_after_the_grace_and_a_light_one_none() {
    let mut app = App::headless_for_test();
    let regrids = message_regrids();
    app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_000)), false);
    assert_eq!(
        app.messages.live_rows().count(),
        0,
        "a light pass paints nothing"
    );
    app.settle_messages(Instant::now() + PROGRESS_GRACE * 2);
    assert_eq!(message_regrids(), regrids);
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
    assert!(
        app.messages.live_rows().next().is_some(),
        "the heavy pass's row"
    );
    assert_eq!(app.message_band_rows, 0, "inside its grace, off the glass");
    assert_eq!(message_regrids(), regrids);
    let mut t = past_the_grace();
    app.settle_messages(t);
    assert_eq!(app.message_band_rows, 1, "revealed once the work lasted");
    assert_eq!(message_regrids(), regrids + 1);
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "a read restates in place"
    );
    assert_eq!(message_regrids(), regrids + 1);
    // The vendor sub-pass answers and ends; the ALab set's reads follow.
    app.record_toolchain_outcome(toolchain_words::ended("the vendor pass finished", false));
    app.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), false);
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_100)), false);
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "latched: nothing folds"
    );
    app.toolchain_pass_ended(true);
    assert_eq!(app.messages.live_rows().count(), 0, "the exit folds it");
    assert_eq!(
        app.messages.echoes().first().map(|e| e.kind),
        Some(EchoKind::Complete)
    );
    for _ in 0..40 {
        t += std::time::Duration::from_millis(100);
        app.settle_messages(t);
    }
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(message_regrids(), regrids + 2, "one open, one fold");
}

/// A PERSON'S CHECK TAKES ITS ROW (design ruling 224): they pressed Check
/// & Update Now and wait for it, so its pass is theirs whatever it weighs
/// — the 250 MB bar is for work nobody asked for. A no-op Check (1–4 s,
/// `packages.log`) ends inside the grace and never touches the glass; a
/// longer one shows its busy row, becomes the plan's fill (a light 40 MB
/// pass included), holds across the markers' records and the pass's ending
/// read, and folds at the child's exit with its Complete echo. Two re-grids.
#[test]
fn a_persons_check_takes_its_row_after_the_grace_and_ends_in_its_words() {
    use crate::message_reporters::{Attention, PackagesVerb, attention};
    let mut quick = App::headless_for_test();
    let regrids = message_regrids();
    let t0 = Instant::now();
    quick.person_pass(PersonPass::Began(PackagesVerb::Check));
    assert_eq!(quick.messages.live_rows().count(), 1, "posted");
    quick.settle_messages(t0 + PROGRESS_GRACE - std::time::Duration::from_millis(50));
    assert_eq!(quick.message_band_rows, 0, "inside its grace");
    quick.person_pass(PersonPass::Ended(EchoKind::Complete));
    quick.settle_messages(t0 + PROGRESS_GRACE * 3);
    assert_eq!(quick.messages.live_rows().count(), 0);
    assert!(quick.messages.echoes().is_empty(), "never shown, no echo");
    assert_eq!(message_regrids(), regrids, "a no-op Check never flashes");

    let mut app = App::headless_for_test();
    let regrids = message_regrids();
    app.person_pass(PersonPass::Began(PackagesVerb::Check));
    let live = app.messages.live_rows().next().expect("the Check's row");
    assert!(live.msg.meter.as_ref().is_some_and(|m| m.busy), "busy");
    assert_eq!(attention(&live.msg), Ok(Attention::Progress));
    let mut t = past_the_grace();
    app.settle_messages(t);
    assert_eq!(app.message_band_rows, 1, "the wait lasted: revealed");
    // The plan lands: a light pass paints, because a person waits.
    app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_000)), false);
    let live = app.messages.live_rows().next().expect("restated");
    assert_eq!(app.messages.live_rows().count(), 1, "in place");
    let meter = live.msg.meter.as_ref().expect("a meter");
    assert!(meter.fill_permille.is_some_and(|f| f > 0), "a fill");
    assert!(meter.amount.is_some(), "the ETA's input");
    assert_eq!(attention(&live.msg), Ok(Attention::Progress));
    // The markers' answer and the ending read fold nothing: the child runs.
    app.record_toolchain_outcome(toolchain_words::ended(
        "the pass finished; 12 ALab program(s) are installed",
        false,
    ));
    app.apply_toolchain_snapshot(Some(&light_read(false, 1_700_000_000)), false);
    app.apply_toolchain_snapshot(None, false);
    assert_eq!(app.messages.live_rows().count(), 1, "the exit's to fold");
    assert!(!app.toolchain_row_latched, "the lane's latch is left alone");
    app.person_pass(PersonPass::Ended(EchoKind::Complete));
    assert_eq!(app.messages.live_rows().count(), 0, "its exit folds it");
    let echo = app.messages.echoes().first().expect("the echo");
    assert_eq!(echo.kind, EchoKind::Complete);
    for _ in 0..40 {
        t += std::time::Duration::from_millis(100);
        app.settle_messages(t);
    }
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(message_regrids(), regrids + 2, "one open, one fold");
    // Nothing is left behind: the next light routine pass is silent again.
    app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_500)), false);
    assert_eq!(app.messages.live_rows().count(), 0, "nobody waits on it");
}

/// A QUEUED PERSON'S ROW DOES NOT KEEP THE HOLDER'S FRAME (ruling 229).
/// While the Check waits at the lock its tailer follows the HOLDER'S
/// file, and the row shows that pass — what they wait behind, with its
/// fill and ETA. The holder's end puts the waiting row back; the lock
/// puts the Check's own busy row back, fill and finished words gone, so a
/// no-op Check ends `ALab tools checked`; a late holder read paints
/// nothing; the Check's own plan (the same pass name, the same second)
/// paints as before.
#[test]
fn a_queued_check_drops_the_holders_frame_when_the_holder_ends() {
    use crate::message_reporters::{PackagesVerb, WAITING_FOR_ALAB};
    let row = |app: &App| {
        app.messages
            .live_by_key(toolchain_words::KEY_PASS)
            .map(|l| {
                let fill = l.msg.meter.as_ref().and_then(|m| m.fill_permille);
                (l.msg.title.clone(), fill, l.msg.finished_title())
            })
            .expect("the person's row")
    };
    let run = |app: &mut App, own_pass_plans: bool| {
        app.person_pass(PersonPass::Began(PackagesVerb::Check));
        app.person_pass(PersonPass::Waiting);
        assert_eq!(row(app).0, WAITING_FOR_ALAB, "queued: it says so");
        assert_eq!(app.messages.live_rows().count(), 1, "one row, restated");
        // The holder (pid 7) plans a light pass: the queued row shows it.
        app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_000)), false);
        let (title, fill, _) = row(app);
        assert_eq!(title, "Updating ALab tools", "what they wait behind");
        assert!(fill.is_some(), "with its fill");
        // The holder ends: its frame leaves, the wait is said again.
        app.apply_toolchain_snapshot(Some(&light_read(false, 1_700_000_000)), false);
        assert_eq!(row(app), (WAITING_FOR_ALAB.to_string(), None, row(app).2));
        app.toolchain_pass_ended(true);
        assert_eq!(
            row(app).0,
            WAITING_FOR_ALAB,
            "the lane's exit is not theirs"
        );
        // A late holder frame lands, then the lock is taken.
        app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_000)), false);
        app.person_pass(PersonPass::Running);
        assert_eq!(
            row(app),
            (
                "Checking ALab tools".to_string(),
                None,
                "ALab tools checked".to_string()
            )
        );
        // Another late holder read, after the lock: nothing.
        app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_000)), false);
        assert_eq!(row(app).0, "Checking ALab tools", "late: paints nothing");
        assert_eq!(row(app).1, None);
        if own_pass_plans {
            // The Check's own plan: another pid, the same pass and second.
            let mut own = light_read(true, 1_700_000_000);
            own.file.pid = Some(9);
            app.apply_toolchain_snapshot(Some(&own), false);
            assert_eq!(row(app).0, "Updating ALab tools", "its own plan paints");
            assert!(row(app).1.is_some());
        }
        app.settle_messages(past_the_grace());
        app.person_pass(PersonPass::Ended(EchoKind::Complete));
        let echo = app.messages.echoes().first().expect("the echo");
        assert_eq!(echo.kind, EchoKind::Complete);
        echo.msg.finished_title()
    };
    let mut noop = App::headless_for_test();
    assert_eq!(run(&mut noop, false), "ALab tools checked");
    assert_eq!(noop.toolchain_holder_pid, None, "nothing carried");
    // With no verb in flight the wait words are not the lane's to say.
    noop.person_pass(PersonPass::Waiting);
    assert_eq!(noop.messages.live_rows().count(), 0);
    let mut planned = App::headless_for_test();
    assert_eq!(run(&mut planned, true), "ALab tools updated");
}

/// A person's install (the minutes-long ~3 GB wait Settings starts) shows the size once
/// atpkg announces it (`10 programs · ~3 GB`), then the plan's fill under the install's
/// own title; a failed one ends in a Fault echo (the page, the Packages badge and the
/// record say why). A Remove is busy with the disk as its load.
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

/// A person's Check behind a first run: the lane's latched row keeps
/// showing the install they wait behind, the person's end leaves it to the
/// lane, and the lane's exit ends it and puts the person's row back (after
/// the grace) if their verb still runs.
#[test]
fn a_check_behind_a_first_run_leaves_the_first_run_its_row() {
    use crate::message_reporters::PackagesVerb;
    let mut app = App::headless_for_test();
    app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    app.person_pass(PersonPass::Began(PackagesVerb::Check));
    app.person_pass(PersonPass::Waiting);
    let title = |app: &App| {
        app.messages
            .live_by_key(toolchain_words::KEY_PASS)
            .map(|l| l.msg.title.clone())
    };
    assert_eq!(title(&app).as_deref(), Some("Installing ALab tools"));
    // The person's own tailer follows the same file (`first_run` false): the lane's tailer
    // paints the first run in the first run's words, never `Updating`, and its echo is not
    // the Check's `ALab tools updated`.
    let mut later = net_read(true, 1_700_000_000);
    later.file.overall.bytes_done += 1_000_000;
    app.apply_toolchain_snapshot(Some(&later), true);
    app.apply_toolchain_snapshot(Some(&later), false);
    assert_eq!(title(&app).as_deref(), Some("Installing ALab tools"));
    let live = app.messages.live_by_key(toolchain_words::KEY_PASS).unwrap();
    assert_eq!(live.msg.finished_title(), "Installed ALab tools");
    app.toolchain_pass_ended(true);
    assert_eq!(
        app.messages.echoes().first().map(|e| e.kind),
        Some(EchoKind::Complete),
        "the first run ended as it did"
    );
    assert_eq!(
        title(&app).as_deref(),
        Some(crate::message_reporters::WAITING_FOR_ALAB),
        "still queued, a moment from the lock"
    );
    app.person_pass(PersonPass::Running);
    assert_eq!(title(&app).as_deref(), Some("Checking ALab tools"));
    // A late read of the first run, from the person's tailer, paints nothing.
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
    assert_eq!(title(&app).as_deref(), Some("Checking ALab tools"));
    app.person_pass(PersonPass::Ended(EchoKind::Complete));
    assert_eq!(app.messages.live_rows().count(), 0);
}

/// A FIRST RUN THAT ENDS SHORT LEAVES A FAILURE ROW (review 2026-09-24):
/// the row told the person to wait for a multi-gigabyte deliverable, so
/// when a failure marker names the end and the child exits unclean, the
/// Fault echo is followed by a row they can act on — `Packages`, the
/// default hold, the cause behind Details. A clean exit, a heavy ROUTINE
/// pass (the installed versions keep working) and a failure with no
/// first run latched keep only the record.
#[test]
fn a_first_run_that_ends_short_leaves_a_failure_row() {
    use crate::toolchain_words::FirstRunShort;
    let failure_rows = |app: &App| {
        app.messages
            .live_rows()
            .filter(|l| l.msg.key.as_deref() == Some(toolchain_words::KEY_PASS))
            .map(|l| (l.msg.title.clone(), l.msg.hold, l.msg.severity))
            .collect::<Vec<_>>()
    };
    // The first run, ended by `seed-failed:` and an unclean exit.
    let mut app = App::headless_for_test();
    app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    app.note_toolchain_first_run_short(FirstRunShort::Failed, "could not reach the index");
    app.toolchain_pass_ended(false);
    assert_eq!(
        app.messages.echoes().first().map(|e| e.kind),
        Some(EchoKind::Fault),
        "the indicator ends as a fault"
    );
    assert_eq!(
        failure_rows(&app),
        vec![(
            toolchain_words::ALAB_INSTALL_FAILED.to_string(),
            Hold::Default,
            Severity::Warn
        )],
        "…and the failure row follows it"
    );
    // An over read after the exit leaves the failure standing.
    app.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), true);
    assert_eq!(failure_rows(&app).len(), 1, "an over read leaves it");
    // A retry's first read replaces it at once — never behind the failure
    // it answers.
    let mut retry = App::headless_for_test();
    retry.announce_toolchain_pass("installing 10 ALab program(s)", true);
    retry.note_toolchain_first_run_short(FirstRunShort::Failed, "offline");
    retry.toolchain_pass_ended(false);
    assert_eq!(failure_rows(&retry)[0].1, Hold::Default, "the failure row");
    retry.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_100)), true);
    let rows = failure_rows(&retry);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        matches!(rows[0].1, Hold::Live { .. }),
        "the retry's progress took the key: {rows:?}"
    );

    // A clean exit after a marker: the record only.
    let mut clean = App::headless_for_test();
    clean.announce_toolchain_pass("installing 10 ALab program(s)", true);
    clean.note_toolchain_first_run_short(FirstRunShort::Failed, "trust failed");
    clean.toolchain_pass_ended(true);
    assert!(
        failure_rows(&clean).is_empty(),
        "a clean exit raises nothing"
    );

    // A heavy routine pass that fails: the record only.
    let mut heavy = App::headless_for_test();
    let mut read = net_read(true, 1_700_000_000);
    read.file.overall.bytes_total = toolchain_words::HEAVY_PASS_BYTES * 2;
    heavy.apply_toolchain_snapshot(Some(&read), false);
    heavy.note_toolchain_first_run_short(FirstRunShort::Failed, "could not reach the index");
    heavy.toolchain_pass_ended(false);
    assert!(
        failure_rows(&heavy).is_empty(),
        "a routine pass's failure keeps the installed versions: a record"
    );

    // No latched first run at all: nothing.
    let mut idle = App::headless_for_test();
    idle.note_toolchain_first_run_short(FirstRunShort::Nothing, "nothing installed");
    idle.toolchain_pass_ended(false);
    assert!(failure_rows(&idle).is_empty());
}

/// A headless App with its first window put on screen (the test seam for a real OS
/// window) and focused: the one place the band moves.
fn on_screen_app() -> App {
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
    ws.band_on_screen_for_test = true;
    ws.focused = true;
    app.system_reduce_motion = false;
    app.serious_mode = false;
    app
}

/// A CHANGE OF LOOK IS A DUE FRAME by itself (review 2026-09-24): a
/// frame on glass drawn in the moving look is due the moment the window's
/// look turns still — focus lost, Serious Mode, Reduce Motion — whether or
/// not the trigger asked for its own redraw, and a still frame is due the
/// moment motion is allowed again. Once re-drawn in the window's look it is
/// due only on its deadline.
#[test]
fn a_frame_drawn_in_another_look_is_due_at_once() {
    let wid = WindowId(0);
    let mut app = on_screen_app();
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let now = Instant::now();
    app.prepare_band_motion(wid, now);
    assert!(
        app.band_motion_due(now).is_empty(),
        "just drawn in its own look: not due yet"
    );
    app.windows.get_mut(&wid).unwrap().focused = false;
    assert_eq!(app.band_look(wid).pace, aterm_messages::Pace::Still);
    assert_eq!(
        app.band_motion_due(now),
        vec![wid],
        "the moving frame on glass is stale in the still look"
    );
    app.prepare_band_motion(wid, now);
    assert!(app.band_motion_due(now).is_empty(), "re-drawn: settled");
    app.windows.get_mut(&wid).unwrap().focused = true;
    assert_eq!(app.band_motion_due(now), vec![wid], "and back again");
}

// The update lane on the band.

fn download(version: &str) -> aterm_update::Progress {
    aterm_update::Progress::Downloading {
        version: version.into(),
        bytes_done: 45_000_000,
        bytes_total: 74_000_000,
    }
}

/// The ready row a press installs from (automatic install off).
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

/// The switch's row: busy, on the glass at once.
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

/// A BUSY ROW'S MOTION MUST NEVER DELAY THE UPDATE IT ANIMATES
/// (2026-09-23; design ruling 56, re-pinned on the engine's motion by
/// ruling 140; the spinner that first carried it went with ruling 251). The flow
/// row is busy while the lane works and the automatic lane waits for quiet
/// moments, so an animation frame that counted as terminal activity would
/// push the very install it is showing. Sixteen comet steps through the
/// host's own frame path (`prepare_band_motion`, one per four frames of
/// the 33 ms grid) move the painted frame and touch no activity clock — not
/// the quiet-epoch stamp, not the handoff epoch, not the keystroke clock
/// — and the lane's quiet verdict at a later instant is the same with or
/// without them. The negative control: a tolerated event (a pointer move)
/// DOES move the stamp, so the reading is not vacuous.
#[test]
fn the_busy_animation_never_touches_the_update_activity_clocks() {
    let wid = WindowId(0);
    let mut app = on_screen_app();
    let now = Instant::now();
    // The switch's row: busy, on the glass at once (no grace).
    post_installing(&mut app, "9.9.9");
    assert!(
        app.messages.busy_on_glass(),
        "PRECONDITION: a busy row is up"
    );
    assert_eq!(app.band_look(wid).pace, aterm_messages::Pace::Moving);
    let clocks = |app: &App| {
        (
            app.last_update_activity_at,
            app.update_handoff_activity_epoch,
            app.last_keystroke_at,
        )
    };
    let before = clocks(&app);
    let later = now + std::time::Duration::from_secs(5);
    let quiet_before = app.automatic_update_activity_quiet_with_pending_input(later, false);
    let revision = app.messages.revision();
    let step = aterm_messages::ANIM_FRAME * 4;
    let mut drawn = std::collections::BTreeSet::new();
    for k in 0..16_u32 {
        drawn.insert(app.prepare_band_motion(wid, now + step * k));
    }
    assert!(
        drawn.len() >= 15,
        "PRECONDITION: the frames moved ({})",
        drawn.len()
    );
    assert_eq!(clocks(&app), before, "an animation frame is not activity");
    assert_eq!(
        app.messages.revision(),
        revision,
        "a frame is paint: Settings ▸ Messages is never republished at the frame rate"
    );
    assert_eq!(
        app.automatic_update_activity_quiet_with_pending_input(later, false),
        quiet_before,
        "the automatic lane reads the terminal exactly as it would without the comet"
    );
    // NEGATIVE CONTROL: real (tolerated) activity does move the stamp.
    std::thread::sleep(std::time::Duration::from_millis(2));
    app.note_update_handoff_tolerated_activity();
    assert_ne!(
        clocks(&app).0,
        before.0,
        "the clock this test reads is live"
    );
}

/// THE BAND ANIMATES ONLY WHILE A BUSY ROW IS COMMITTED AND NOTHING IS
/// FROZEN (the gating union, ruling 140; focus, visibility and the still
/// looks are `band_motion_tests`'): a busy row queued behind the committed
/// rows does not animate; and under a handoff freeze the band is still and
/// arms and asks for nothing — the frame on glass stays WHERE IT IS, not even
/// a switch to still repaints the parked screen.
#[test]
fn the_band_animates_only_while_committed_focused_moving_and_unfrozen() {
    use aterm_messages::Pace;
    let wid = WindowId(0);
    let mut app = on_screen_app();
    let now = Instant::now();
    assert_eq!(app.band_motion_deadline(now), None, "no row");
    // A busy row with no grace to wait out: the switch's.
    app.post_message(update_words::installing("9.9.9"));
    assert_eq!(app.band_look(wid).pace, Pace::Moving);
    assert!(app.band_motion_deadline(now).is_some());
    // Frozen: still, nothing armed, nothing due — the frame stays.
    let at = now + aterm_messages::ANIM_FRAME * 12;
    let fp = app.prepare_band_motion(wid, at);
    assert_ne!(fp, 0, "PRECONDITION: a moving frame is on glass");
    app.incoming_handoff_pending = true;
    assert_eq!(app.band_look(wid).pace, Pace::Still, "frozen: still");
    assert_eq!(app.band_motion_deadline(at), None, "frozen: nothing armed");
    assert!(
        app.band_motion_due(at + aterm_messages::ANIM_FRAME * 30)
            .is_empty(),
        "frozen: nothing due, even in another look"
    );
    assert_eq!(app.windows[&wid].band.motion_fp(), fp, "frozen where it is");
    app.incoming_handoff_pending = false;
    // Queued behind three Warn rows: a row the band does not show does
    // not move.
    let mut queued = on_screen_app();
    for n in 0..3 {
        queued.post_message(Message::new(tags::CONFIG, Severity::Warn, format!("w{n}")));
    }
    queued.note_update_progress(&aterm_update::Progress::Verifying {
        version: "9.9.9".into(),
    });
    let cols = usize::from(queued.windows[&wid].cols);
    let p = queued.band_presentation(cols);
    let busy_shown = p.rows.iter().any(|r| match r.kind {
        aterm_messages::RowKind::Message(id) => {
            queued.messages.live(id).is_some_and(|l| l.is_busy())
        }
        _ => false,
    });
    assert!(
        !busy_shown,
        "PRECONDITION: the busy row is queued behind the warnings"
    );
    assert_eq!(
        queued.band_motion_deadline(now),
        None,
        "a busy row the band does not show does not move"
    );
}

/// ONE UPDATE COSTS ONE GROW AND ONE SHRINK (the regrid budget, measured):
/// download → check → the ready row → the press's installing are
/// restatements or supersedes of ONE row, and forty animation frames are
/// paint — one grow for all of it; the landing's fold after the D1 quiet is
/// the one shrink.
#[test]
fn an_update_flow_costs_one_grow_and_one_shrink() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let regrids = message_regrids();
    app.note_update_progress(&download("9.9.9"));
    app.note_update_progress(&aterm_update::Progress::Verifying {
        version: "9.9.9".into(),
    });
    post_ready_staged(&mut app, "9.9.9", 7);
    app.begin_update_installing(true);
    let frames_from = Instant::now();
    for k in 0..40_u32 {
        app.prepare_band_motion(WindowId(0), frames_from + aterm_messages::ANIM_FRAME * k);
    }
    assert_eq!(app.message_band_rows, 1);
    assert_eq!(message_regrids(), regrids + 1, "one grow");
    app.post_update_landed("9.9.9", 7, 0);
    let now = Instant::now();
    let _ = app.settle_messages(now + aterm_messages::HOLD_SUCCESS);
    let _ = app.settle_messages(
        now + aterm_messages::HOLD_SUCCESS
            + aterm_messages::SHRINK_QUIET
            + std::time::Duration::from_millis(1),
    );
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(message_regrids(), regrids + 2, "and one shrink");
}

/// Unsaved editor work holding the install is a row of its own, painted: the one blocker
/// only the person can clear. Where the lane retries by itself the row stands (the wait
/// has no deadline); a press nothing retries keeps the transient outcome row.
#[test]
fn an_editor_holding_the_install_is_a_row_of_its_own() {
    let mut app = App::headless_for_test();
    app.note_update_blockers(&[App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()], true);
    let outcome = app
        .messages
        .live_by_key(update_words::KEY_WAITS_FOR_YOU)
        .expect("the row");
    assert!(
        matches!(outcome.msg.hold, Hold::Standing(_)),
        "the wait's one notice stands: {:?}",
        outcome.msg.hold
    );
    assert_eq!(
        outcome.msg.detail[0],
        App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
        "the blocker is what the person does: painted"
    );
    assert!(outcome.msg.excerpt);
    assert_eq!(outcome.msg.severity, Severity::Warn);
    assert_eq!(app.messages.live_rows().count(), 1, "one row");
    // THE WORDS FIT WHERE A TERMINAL IS (2026-09-24): no capsule takes the
    // columns the instruction needs. (A standing row is born a progress
    // grace late.)
    let _ = app.settle_messages(
        Instant::now() + aterm_messages::PROGRESS_GRACE + std::time::Duration::from_millis(1),
    );
    for cols in [80, 60] {
        let row = app.band_presentation(cols).rows[0].clone();
        assert!(
            row.detail.is_some(),
            "{cols} columns: the instruction shows"
        );
        assert!(
            row.capsules.iter().all(|c| c.action.is_details()),
            "{cols} columns: only `Details ›`"
        );
    }
    // A press nothing retries: the transient outcome row, as before.
    let mut pressed = App::headless_for_test();
    pressed.note_update_blockers(&[App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()], false);
    let transient = pressed
        .messages
        .live_by_key(update_words::KEY_OUTCOME)
        .expect("the press's row");
    assert_eq!(transient.msg.hold, Hold::Default);
    assert!(
        pressed
            .messages
            .live_by_key(update_words::KEY_WAITS_FOR_YOU)
            .is_none()
    );
}

/// A held update row replaced by a newer one is resolved on record: one outcome row
/// replaced by another leaves a finished activity on `appstatus` (a superseded row would
/// record nothing). A failed download is a record of its own (a self-retry), and the live
/// download row it ends is resolved `Warn`, on record too.
#[test]
fn a_held_update_row_replaced_by_a_newer_one_is_resolved_on_record() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    // A person asked (Check for Updates): the download is their wait and takes the row.
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

/// The landing is on record with how long the update took, from this build's finished
/// download carried across the handoff; never for a build met already on disk, nor for
/// another build's download.
#[test]
fn the_landing_is_on_record_with_how_long_the_update_took() {
    // One landing, one entry: the duration is a line of the landing record, never a
    // second record.
    let took = |app: &App| {
        app.messages
            .log()
            .records()
            .flat_map(|r| r.detail.iter())
            .filter(|l| l.contains("after the download"))
            .cloned()
            .collect::<Vec<_>>()
    };
    // The carry: the parent's instant on the wall clock, the successor's
    // on its own monotonic clock.
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
        said[0] == "installed 42 s after the download"
            || said[0] == "installed 43 s after the download",
        "{said:?}"
    );
    // Negative controls: met on disk (no instant), another build's instant,
    // no carried instant.
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

/// ONE ANNOUNCEMENT PER TITLE PER LAUNCH, WHOEVER RAISES IT (the review of
/// the update audit's merge onto main's health door): the updater's
/// streak, its overdue notice and the automatic lane's convergence all say
/// "Couldn't install updates". The first is a row and owed the OS
/// banner (`true`); the same title again, unhealed, posts nothing and owes
/// nothing — also after the row folded. A different class still speaks, and
/// a healing re-opens the title for a new episode.
///
/// RED before the latch moved to this door: the second call returned
/// `true` and re-posted the row (and, in the caller, a second banner).
#[test]
fn a_failing_title_is_announced_once_whoever_raises_it() {
    let apply = aterm_update::health_failing_title("apply");
    let streak = "3 failed attempts to install in a row since 2026-09-24T01:00:00Z: why.";
    let overdue = "build 9 has been waiting on this machine since 2026-09-24T00:00:00Z and \
                   build 8 is still running: why.";
    let mut app = App::headless_for_test();
    band_seen(&mut app);
    assert!(app.note_update_health(apply, "automatic apply of build 9 stopped"));
    let revision = app.messages.revision();
    assert!(
        !app.note_update_health(apply, overdue),
        "the overdue notice, same title"
    );
    assert_eq!(app.messages.revision(), revision, "no second row");
    let _ = app.settle_messages(
        Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(1),
    );
    assert!(app.messages.live_health().is_none(), "folded");
    assert!(
        !app.note_update_health(apply, streak),
        "folded is still said"
    );
    assert!(app.messages.live_health().is_none());
    assert!(
        app.note_update_health(aterm_update::health_failing_title("manifest"), streak),
        "another class is other news"
    );
    assert!(
        !app.note_update_health(apply, overdue),
        "and does not re-open this one"
    );
    assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, "manifest"));
    assert!(
        !app.note_update_health(apply, overdue),
        "a healed download streak does not answer an install warning"
    );
    assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, "apply"));
    assert!(
        app.note_update_health(apply, streak),
        "a healed title speaks again"
    );
}

/// A warning said again in new words (its promised retry withdrawn): the row up takes them
/// in place, no second row; once it has folded, a record in them follows, so Messages
/// keeps no withdrawn promise.
#[test]
fn a_restated_health_warning_takes_its_new_words_where_it_stands() {
    let apply = aterm_update::health_failing_title("apply");
    let kind = update_words::HealthKind::of_updater_title(apply);
    let words = |app: &App| app.messages.live_health().map(|l| l.msg.detail.join(" "));
    let mut app = App::headless_for_test();
    band_seen(&mut app);
    assert!(app.note_update_health(apply, "it didn't install; it tries again in 24 h"));
    let rows = app.messages.live_rows().count();
    app.restate_update_health(
        kind,
        apply,
        "it didn't install; a newer release replaces it",
    );
    assert_eq!(
        words(&app).as_deref(),
        Some("it didn't install; a newer release replaces it")
    );
    assert_eq!(app.messages.live_rows().count(), rows, "no second row");
    let _ = app.settle_messages(
        Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(1),
    );
    assert!(words(&app).is_none(), "folded");
    app.restate_update_health(kind, apply, "it didn't install; nothing more comes");
    let record = app.messages.log().records().next_back().expect("a record");
    assert_eq!(
        record.detail.join(" "),
        "it didn't install; nothing more comes"
    );
}

/// The ledger's heal proves the class it names, not every warning: an install warning the
/// convergence or overdue notice raised is not answered by the updater healing a download
/// streak. The heal of the apply class, or a heal that names no class (an older
/// updater's), still answers it.
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

/// A WARNING HEALS ONLY ON THE PROOF THAT ANSWERS IT: the title names the
/// broken half, so a download never records "aterm updates work again"
/// over "Couldn't install updates" — the updater's ledger keeps an
/// install streak through a whole-pipeline success. Checking is answered by
/// a download starting (downloading only by a staged build:
/// `the_heal_is_keyed_by_kind_and_an_older_builds_words_still_heal`;
/// installing only by a landing:
/// `two_health_kinds_live_each_record_their_own_healing`).
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
    // Check: a download starting answers it.
    let mut check = App::headless_for_test();
    assert!(check.note_update_health(aterm_update::health_failing_title("manifest"), body));
    check.note_update_progress(&download("9.9.9"));
    assert!(!warning_up(&check));
    assert_eq!(healed(&check), 1);
}

/// A CARRIED WARNING HEALED BY THE LANDING IS ON THE SUCCESSOR'S RECORD (round
/// six, finding 54): the parent `_exit`s at Commit and never learns its warning
/// healed, so what it said crosses the handoff as its own field — surviving the
/// manifest's own encoding — and the successor's landing records "aterm
/// updates work again" once, against the parent's words: on the explicit lane
/// (whose seed resolves the carried row with the handoff) and the automatic one
/// (no progress row carried), the warning still up or already gone from the
/// glass. A plain restart remembers nothing, and a carry with nothing to heal
/// writes no key.
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
            let crossed = through_the_wire(&window_carry(
                parent.message_band_rows,
                parent.carried_messages(),
            ));
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
    // A plain restart remembers nothing; a carry with nothing to heal
    // writes no key (the wire it always was).
    let mut plain = App::headless_for_test();
    plain.seed_carried_messages(&carry, None);
    plain.post_update_landed("0.76.0", 7, 0);
    assert!(healed(&plain).is_empty());
    let quiet = window_carry(0, App::headless_for_test().carried_messages());
    let text = aterm_toml::to_string(&quiet).expect("encodes");
    assert!(!text.contains("update_health_said"), "{text}");
}

/// The heal is keyed by kind, not by the title's words: a row an older build raised under
/// the family key in its own words is read back to its kind, healed only by the proof that
/// answers it, and replaced when this build says the same warning again.
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
    // An older build's row: the family key and its own words.
    let older = |title: &str| {
        let mut msg = update_words::health_warning(update_words::HealthKind::Check, title, body);
        msg.key = Some(update_words::KEY_HEALTH.to_string());
        msg
    };
    let carried = |title: &str| {
        let mut parent = App::headless_for_test();
        parent.post_message(older(title));
        let carry = window_carry(parent.message_band_rows, parent.carried_messages());
        let mut app = App::headless_for_test();
        app.seed_carried_messages(&carry, None);
        assert_eq!(health_rows(&app).len(), 1, "{title}: carried");
        app
    };

    // Download, in the old words: a download starting is no proof…
    let mut app = carried("aterm can't download updates");
    app.note_update_progress(&download("9.9.9"));
    assert_eq!(
        health_rows(&app).len(),
        1,
        "NEGATIVE: the old words are the download half, not the check half"
    );
    // …a staged build is.
    app.note_update_progress(&staged);
    assert!(health_rows(&app).is_empty(), "healed by a staged build");

    // Install, in the old words: only a landing answers it.
    let mut app = carried("aterm can't install updates");
    app.note_update_progress(&download("9.9.9"));
    app.note_update_progress(&staged);
    assert_eq!(health_rows(&app).len(), 1, "NEGATIVE: not by a download");
    app.post_update_landed("9.9.9", 7, 0);
    assert!(health_rows(&app).is_empty(), "healed by the landing");

    // The watchdog's stall, in its old words: a completed check heals it.
    let mut app = carried("aterm's update check stopped");
    app.heal_update_checker_stall();
    assert!(health_rows(&app).is_empty(), "the stall's own heal");

    // The same warning said again by this build replaces the carried one:
    // one row, its kind's key, today's words.
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

    // The key decides, not the words: an install warning worded like a download is not
    // healed by a staged build.
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

/// Two failing halves are two rows, each healed on record by its own proof in its own
/// words: a later kind never takes an earlier one's healing, and each record is written
/// once.
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

    // Download, then Install: the staged build answers the download only.
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

    // Install, then Download: the landing answers the install only (the build may have
    // been staged before the download streak began, and the ledger keeps it); a staged
    // build then answers the download. Both are on record, each against its own words.
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

    // Install, then the watchdog's stall: a completed check records the
    // stall's healing and leaves the install warning, words and all.
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

/// EVERY UPDATE MOMENT IS A LINE IN THE LOG (the log area's P3): downloaded,
/// the switch begun, a stopped automatic attempt, the landing — each a record
/// or a supersede the log keeps, in order; a restatement alone writes no
/// line, which is why the lane POSTS each phase.
#[test]
fn every_update_moment_is_a_line_in_the_log() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    // A person asked (Check for Updates): the download is their wait and takes the row.
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
        // The automatic lane's second attempt at the same build: aterm.log's.
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

/// One fact, one entry at the switch: an attempt that posted its own Installing row has
/// that line in the log already, so the switch record is not written beside it; with no
/// row up (the automatic lane) the record is the attempt's only line.
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
    // …and the attempt is still on record for its stop.
    explicit.record_update_switch_stopped(false, "the proof timed out");
    assert_eq!(explicit.messages.log().len(), before + 1);
}

/// A HARNESS NOTE IS RECORDED AND NEVER TAKES A ROW (design ruling 61; its
/// `kind=harness` line is the engine's `wire` test): no row, no re-grid, no
/// wake; the same note again is not written again; beside a first-run row it
/// leaves that one row alone.
#[test]
fn a_harness_note_is_recorded_and_never_takes_a_row() {
    let mut app = App::headless_for_test();
    let regrids = message_regrids();
    assert_eq!(
        app.take_app_notice("harness", "aterm harness acting \u{00b7} approving rm"),
        Ok(crate::AppNoticeTaken::Recorded)
    );
    assert_eq!(app.messages.live_rows().count(), 0);
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(app.messages_deadline(), None);
    assert_eq!(message_regrids(), regrids);
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

/// An unchanged managed-current marker is recorded once; news is recorded again: atpkg
/// prints it every pass and the shared ring is for news, so a moved version is a new
/// record, and a frozen tab counted re-words the record already written, in place.
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

// ---- The wire's verbs: `notice` and `messages` -----------------------------

/// One `notice` line through the control thread's parse and the main
/// thread's apply, as `cmd_notice` runs them.
fn notice(app: &mut App, line: &str) -> String {
    let req = wire::NoticeRequest::parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    app.take_notice(req)
}

/// The id an `OK …=<id>` reply names.
fn replied_id(reply: &str, field: &str) -> MessageId {
    let raw = reply
        .split_whitespace()
        .find_map(|w| w.strip_prefix(field))
        .unwrap_or_else(|| panic!("{reply:?} has no {field}"));
    MessageId::from_raw(raw.parse().expect("a number")).expect("a nonzero id")
}

/// A warn post through the host: the reply names the row and the band
/// commits it (the rows `messages` reads back are the engine's `wire` tests');
/// an info post is a record that moves no band.
#[test]
fn a_notice_round_trips_through_the_band() {
    let mut app = App::headless_for_test();
    let reply = notice(
        &mut app,
        "post script sev=warn key=deploy Deploy failed -- connection refused",
    );
    assert!(reply.starts_with("OK message="), "{reply}");
    let id = replied_id(&reply, "message=");
    let live = app.messages.live(id).expect("a warn post is a row");
    assert_eq!(live.msg.origin, aterm_messages::Origin::Wire);
    assert_eq!(app.message_band_rows, 1, "the row is on the band");
    // An info post is a record: no row, nothing on the band.
    let reply = notice(&mut app, "post script hello from a script");
    assert!(reply.starts_with("OK recorded message="), "{reply}");
    let rec = replied_id(&reply, "message=");
    assert!(app.messages.live(rec).is_none());
    assert_eq!(app.message_band_rows, 1);
}

/// A progress flood is one row per key, and the one paced paint left over is
/// in the loop's deadline and taken by its settle.
#[test]
fn a_wire_progress_flood_repaints_at_most_once_a_frame() {
    let mut app = App::headless_for_test();
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
    // Arm a paced paint (a line inside a frame of the last paint).
    let mut n = 0_u32;
    while app.wire_gate.due().is_none() && n < 64 {
        n += 1;
        let _ = notice(
            &mut app,
            &format!("progress build done={n}/100 Building aterm"),
        );
    }
    let due = app.wire_gate.due().expect("a paced paint is armed");
    let deadline = app.messages_deadline().expect("the loop is due back");
    assert!(deadline <= due, "the deadline covers the paced paint");
    assert!(app.settle_messages(due), "the paced paint repaints");
    assert_eq!(app.wire_gate.due(), None, "taken once");
}

/// `notice act` is the page's own press: an index answers an ask (`Not
/// now` retires it, the press is on record), a FULL label presses a
/// navigation, `details` marks a row seen; a press the row lacks and an
/// id with no live row are refused with the engine's words.
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
    // A navigation by its full label.
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
    // `details` on a live progress row marks it seen.
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

// ---- the second adversarial hunt's host repros -------------
//
// Each states the EXPECTED behaviour its candidate names and failed on
// the code it was written against; the fix stage makes each pass.

/// host-msgs-R44-HOST-2 — rulings 28, 367 and C10 (§10.3): a replace
/// resolves only what the reload re-derived, and `config.font-family` (the
/// backend thread's own verdict, its own key) is re-derived by no reload. A
/// clean full reload that did no font work must leave the rejected family's
/// row up, never close it `Font applied` ✓ — and a row the person dismissed
/// keeps its title and its ⚠ through it.
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

/// A reload whose font work ran does re-derive the family (the catalog worker admits it,
/// or says `config font_family: …` under `config.fonts`), so one that admitted it answers
/// the launch thread's rejection: its row closes fixed. One that rejected the family
/// again leaves the row: its problem stands.
#[test]
fn r44_review_a_reload_whose_font_work_admits_the_family_closes_its_rejection() {
    fn reload(app: &mut App, dir: &std::path::Path, warnings: Vec<String>) {
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "# font_family cleared\n").expect("write config");
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
    // Rejected again by the reload's own font work: the row stands.
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
    // Admitted: the launch's rejection is answered.
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

/// host-msgs-R44-HOST-3 — rulings 265/270: a retired record never offers
/// an intent that acted on the live row (`LogRecord::still_offers`, the
/// page's and the wire's rule). A stale page press on a retired row's
/// `Show tab 2`, a retired paste row's `Stop paste`, or a retired
/// recovery row's `End sessions`, is `Gone`: no tab switched, nothing raised.
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

/// AN UPDATE WARNING SAID WHILE NO WINDOW IS ON SCREEN IS SHOWN WHEN ONE IS,
/// AS ONE ENTRY (U37 and its review). The warning held its 45 s
/// from the post whether or not any window's band was on screen (minimized,
/// fully covered, or no OS window at all), the kind stays latched for the
/// launch, and no OS banner stands in for it unless desktop alerts are on — so a
/// stuck update announced while nobody was looking folded unseen and was never
/// shown again in that process. Then it was posted again once seen, and
/// Settings ▸ Messages listed it twice: the fold nobody saw and the row shown.
/// It waits on the band however long nobody looks, and is one record.
///
/// Driven headlessly: a window whose band is off screen, then on screen
/// (`band_on_screen_for_test`, the seam `band_on_screen` reads).
#[test]
fn an_update_warning_said_with_no_window_on_screen_is_shown_when_one_is() {
    let title = aterm_update::health_failing_title("apply");
    let kind = update_words::HealthKind::of_updater_title(title);
    let body = "build 12 has been waiting on this machine since 2026-10-16T08:00:00Z and \
                build 11 is still running: no apply attempt has been recorded for it.";
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    app.windows.get_mut(&wid).unwrap().occluded = true;
    assert!(
        !band_on_screen(&app.windows[&wid]),
        "PRECONDITION: nobody can see the band"
    );
    assert!(app.note_update_health(title, body), "announced");
    assert_eq!(app.live_update_health(|k| k == kind).len(), 1);
    // Far past its hold, and any queue patience, while nobody looks.
    let unseen = Instant::now() + std::time::Duration::from_secs(10 * 60);
    app.settle_messages(unseen);
    assert_eq!(
        app.live_update_health(|k| k == kind).len(),
        1,
        "the warning waits while nobody can see it"
    );
    // The window comes on screen.
    app.windows.get_mut(&wid).unwrap().occluded = false;
    let back = unseen + std::time::Duration::from_secs(1);
    app.settle_messages(back);
    assert_eq!(
        app.live_update_health(|k| k == kind).len(),
        1,
        "the warning is shown once a window can show it"
    );
    app.settle_messages(back + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(5));
    assert!(
        app.live_update_health(|k| k == kind).is_empty(),
        "PRECONDITION: it folded after its hold"
    );
    let entries = app
        .messages
        .log()
        .records()
        .filter(|rec| rec.key.as_deref() == Some(kind.key()))
        .count();
    assert_eq!(entries, 1, "one entry in Settings ▸ Messages");
}

/// …AND ONE QUEUED BEHIND ROWS THAT HOLD THE BAND IS ONE ENTRY TOO (U37's
/// second review). Three standing errors hold every slot, so the warning said
/// while nobody can see the band waits in the queue, where its patience ran
/// from the post: it left unseen after a minute, and once a window came on
/// screen it was posted again, a second entry under the same key. Unseen, its
/// patience starts again at every settle; seen, it reaches the band when a slot
/// frees, folds after its hold, and is one record. Negative control: without
/// the restart it is gone from the queue before anyone looks.
#[test]
fn an_owed_update_warning_queued_behind_the_band_is_one_entry() {
    use aterm_messages::Hold;
    let title = aterm_update::health_failing_title("apply");
    let kind = update_words::HealthKind::of_updater_title(title);
    let body = "build 12 has been waiting on this machine since 2026-10-16T08:00:00Z and \
                build 11 is still running: no apply attempt has been recorded for it.";
    let secs = std::time::Duration::from_secs;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    app.windows.get_mut(&wid).unwrap().occluded = true;
    let said = Instant::now();
    let standing: Vec<MessageId> = (0..=aterm_messages::MAX_ROWS)
        .map(|n| {
            app.post_message(
                Message::new(tags::RENDER, Severity::Error, format!("standing {n}"))
                    .hold(Hold::STANDING),
            )
        })
        .collect();
    assert!(app.note_update_health(title, body), "announced");
    app.settle_messages(said + secs(1));
    let original = app.live_update_health(|k| k == kind)[0];
    let warning = |app: &App| {
        app.live_update_health(|k| k == kind)
            .first()
            .and_then(|id| app.messages.live(*id))
            .map(aterm_messages::Live::is_queued)
    };
    assert_eq!(
        warning(&app),
        Some(true),
        "PRECONDITION: the standing rows hold the band and the warning is queued"
    );
    for minute in 1..=10 {
        app.settle_messages(said + secs(60 * minute));
    }
    assert_eq!(
        warning(&app),
        Some(true),
        "it waits in the queue while nobody can see the band"
    );
    app.windows.get_mut(&wid).unwrap().occluded = false;
    let back = said + secs(10 * 60 + 1);
    app.settle_messages(back);
    let mut later = back;
    // A visible band still occupied by standing errors has shown none of
    // this warning. Cross its actual patience deadline repeatedly: it must
    // remain the same queued record, not retire and silently discharge owed.
    for _ in 0..3 {
        later = later.max(
            app.messages
                .live(original)
                .expect("same queued warning")
                .patience_at,
        ) + secs(1);
        app.settle_messages(later);
        assert_eq!(warning(&app), Some(true));
        assert_eq!(app.live_update_health(|k| k == kind), vec![original]);
    }
    let freed = later + secs(1);
    for id in standing {
        assert!(app.messages.withdraw(id, freed));
    }
    app.settle_messages(freed + secs(1));
    assert_eq!(warning(&app), Some(false), "a freed slot shows it");
    // Wait on the actual pending shrink before posting a new blocker. With
    // spare glass slots the center preserves the warning beside the errors;
    // a one-row band makes the first higher-ranked row demote it.
    let requeue = app
        .messages
        .quiet_shrink_at()
        .expect("one-row shrink is pending")
        .max(app.band_regrow_floor().unwrap_or(freed));
    app.settle_messages(requeue);
    assert_eq!(
        app.messages.committed_rows(),
        1,
        "the band shrank before the new errors"
    );
    // After being seen briefly, higher-priority rows can queue it again.
    // Previously seen is not permission to spend a queued row's patience.
    let standing: Vec<MessageId> = (0..=aterm_messages::MAX_ROWS)
        .map(|n| {
            app.post_unsynced_at(
                Message::new(tags::RENDER, Severity::Error, format!("later standing {n}"))
                    .hold(Hold::STANDING),
                wall_stamp_now(),
                requeue,
            )
        })
        .collect();
    app.settle_messages(requeue);
    assert_eq!(
        warning(&app),
        Some(true),
        "the partly seen warning is requeued"
    );
    for _ in 0..3 {
        later = later.max(
            app.messages
                .live(original)
                .expect("same requeued warning")
                .patience_at,
        ) + secs(1);
        app.settle_messages(later);
        assert_eq!(warning(&app), Some(true));
        assert_eq!(app.live_update_health(|k| k == kind), vec![original]);
    }
    let freed = later + secs(1);
    for id in standing {
        assert!(app.messages.withdraw(id, freed));
    }
    app.settle_messages(freed + secs(1));
    assert_eq!(warning(&app), Some(false), "a freed slot shows it again");
    // The drive put it on glass after the preceding keep_owed_rows sweep.
    // Let the next sweep credit that visible row before testing its full hold.
    let shown = freed + secs(2);
    app.settle_messages(shown);
    assert_eq!(warning(&app), Some(false), "credited on glass");
    // Withdrawal can rebalance immediately, before the host's next sweep.
    // Read the actual anchored hold rather than guess which sweep placed it.
    let fold_at = app
        .messages
        .live(original)
        .expect("the original warning is visible")
        .fold_at
        .expect("the visible warning has a hold");
    assert!(fold_at >= freed + aterm_messages::HOLD_WARN);
    app.settle_messages(fold_at - secs(1));
    assert_eq!(
        warning(&app),
        Some(false),
        "its whole visible hold remains available"
    );
    app.settle_messages(fold_at + secs(5));
    assert_eq!(
        warning(&app),
        None,
        "PRECONDITION: it folded after its hold"
    );
    app.settle_messages(fold_at + secs(6));
    let entries = app
        .messages
        .log()
        .records()
        .filter(|rec| rec.key.as_deref() == Some(kind.key()))
        .count();
    assert_eq!(entries, 1, "one entry in Settings ▸ Messages");
}

/// …A ROW STILL UP WHEN THE WINDOW COMES BACK IS SEEN FOR ITS WHOLE HOLD, AND
/// NOTHING IS OWED TWICE (U37's controls). Said unseen, the row is still up
/// when the window comes on screen 30 s later: it takes its 45 s again from
/// there (it would have folded at 45 s), folds after them, and is not shown a
/// third time. A warning said ON screen folds after its hold and is not raised
/// again; one healed while unseen is owed nothing.
#[test]
fn an_owed_update_warning_is_shown_once_and_only_while_unanswered() {
    let title = aterm_update::health_failing_title("apply");
    let kind = update_words::HealthKind::of_updater_title(title);
    let body = "build 12 has been waiting on this machine since 2026-10-16T08:00:00Z and \
                build 11 is still running: no apply attempt has been recorded for it.";
    let secs = std::time::Duration::from_secs;
    let on_screen = |app: &mut App, seen: bool| {
        let ws = app.windows.get_mut(&WindowId(0)).unwrap();
        ws.band_on_screen_for_test = true;
        ws.occluded = !seen;
    };
    let live = |app: &App| app.live_update_health(|k| k == kind).len();

    // Unseen, then seen while the row is still up.
    let mut app = App::headless_for_test();
    on_screen(&mut app, false);
    let said = Instant::now();
    assert!(app.note_update_health(title, body));
    on_screen(&mut app, true);
    app.settle_messages(said + secs(30));
    app.settle_messages(said + secs(60));
    assert_eq!(live(&app), 1, "the hold runs from when it could be seen");
    app.settle_messages(said + secs(30 + 45 + 5));
    assert_eq!(live(&app), 0, "and folds after it");
    app.settle_messages(said + secs(30 + 45 + 6));
    assert_eq!(live(&app), 0, "an owed warning is shown once");

    // Said on screen: nothing is owed.
    let mut app = App::headless_for_test();
    on_screen(&mut app, true);
    let said = Instant::now();
    assert!(app.note_update_health(title, body));
    app.settle_messages(said + secs(10 * 60));
    assert_eq!(live(&app), 0, "PRECONDITION: it folded after its hold");
    app.settle_messages(said + secs(10 * 60 + 1));
    assert_eq!(live(&app), 0, "a warning that was seen is not raised again");

    // Healed while unseen: owed nothing.
    let mut app = App::headless_for_test();
    on_screen(&mut app, false);
    let said = Instant::now();
    assert!(app.note_update_health(title, body));
    app.settle_messages(said + secs(10 * 60));
    app.heal_update_health_where(|k| k == kind);
    on_screen(&mut app, true);
    app.settle_messages(said + secs(10 * 60 + 1));
    assert_eq!(live(&app), 0, "a healed warning is owed nothing");
}

/// AN OWED WARNING GOES WITH THE HANDOFF, AND WAITS FOR A BAND THAT IS SEEN (the
/// reviews of , U37). A check warning said while no band was on screen
/// is carried to the successor, which takes the warning up — its updater adopts the
/// class as announced, so it is not said again — and owes it to its own windows. A
/// handoff boot's windows start hidden and not yet occluded (`pending_reveal`), and
/// `band_on_screen` counted them, so the warning was shown at the successor's first
/// settle on a window nobody could see; when the carried stack then minimized the
/// window, or it turned out covered, the row folded unseen and was owed no more. A
/// hidden window's band is not on screen, and an owed row seen for part of its hold and
/// then hidden waits again, for its whole hold once seen. Driven headlessly: the
/// parent's band off screen, the successor's hidden, minimized, then on.
#[test]
fn an_owed_update_warning_is_shown_by_the_successor_it_is_carried_to() {
    let title = aterm_update::health_failing_title("manifest");
    let kind = update_words::HealthKind::of_updater_title(title);
    let body = "Update checks have failed 3 times since 2026-10-16T08:00:00Z: the release \
                channel did not answer.";
    let secs = std::time::Duration::from_secs;
    let on_screen = |app: &mut App, seen: bool| {
        let ws = app.windows.get_mut(&WindowId(0)).unwrap();
        ws.band_on_screen_for_test = true;
        ws.occluded = !seen;
    };
    let live = |app: &App| app.live_update_health(|k| k == kind).len();
    let mut parent = App::headless_for_test();
    on_screen(&mut parent, false);
    assert!(parent.note_update_health(title, body), "announced");
    parent.settle_messages(Instant::now() + secs(10 * 60));
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());

    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("0.76.0"));
    successor.messages.after_handoff_commit(Instant::now());
    on_screen(&mut successor, true);
    let boot = Instant::now();
    successor
        .windows
        .get_mut(&WindowId(0))
        .unwrap()
        .pending_reveal = Some(boot + secs(5));
    assert!(
        !band_on_screen(&successor.windows[&WindowId(0)]),
        "a hidden boot window's band is not on screen"
    );
    successor.settle_messages(boot + secs(1));
    // The carried stack minimizes the window as it is revealed.
    successor
        .windows
        .get_mut(&WindowId(0))
        .unwrap()
        .pending_reveal = None;
    on_screen(&mut successor, false);
    successor.settle_messages(boot + secs(10 * 60));
    assert_eq!(
        live(&successor),
        1,
        "the warning waits for a band that is seen"
    );
    // Seen for ten seconds, then covered: it waits again.
    on_screen(&mut successor, true);
    let seen = boot + secs(10 * 60 + 1);
    successor.settle_messages(seen);
    on_screen(&mut successor, false);
    successor.settle_messages(seen + secs(10));
    successor.settle_messages(seen + secs(10 * 60));
    assert_eq!(
        live(&successor),
        1,
        "seen for part of its hold, it waits again"
    );
    on_screen(&mut successor, true);
    let again = seen + secs(10 * 60 + 1);
    successor.settle_messages(again);
    successor.settle_messages(again + secs(40));
    assert_eq!(live(&successor), 1, "its whole hold once seen");
    successor.settle_messages(again + secs(45 + 5));
    assert_eq!(live(&successor), 0, "and then it folds");
}

/// THE LINUX WAIT FOR A START IS A NOTICE, NEVER A HEALTH WARNING (the review of
/// , U21). On Linux every automatic install waits for the next
/// aterm a person starts, so the updater's notice of that wait
/// (`aterm_update::INSTALL_AWAITS_START_TITLE`) is an Info row under its own key:
/// no banner is owed, no kind is latched, no row reads as a health warning, and
/// no digest line is claimed, so a day later nothing says updates are not
/// installing. Its end (`aterm_update::INSTALL_WAIT_OVER_TITLE`) takes it down.
/// Negative control: a crash loop's rollback after it is the install half's
/// warning, in the rollback's own words.
#[test]
fn a_linux_install_waiting_for_a_start_is_a_notice_and_its_end_takes_it_down() {
    let body = "aterm 0.11.0 was installed over an hour ago and has not started yet: the next \
                aterm you start finishes the update, and update checks wait until then.";
    let mut app = App::headless_for_test();
    app.windows
        .get_mut(&WindowId(0))
        .unwrap()
        .band_on_screen_for_test = true;
    assert!(
        !app.note_update_health(aterm_update::INSTALL_AWAITS_START_TITLE, body),
        "no banner is owed"
    );
    let row = app
        .messages
        .live_by_key(update_words::KEY_AWAITS_START)
        .expect("the notice is on the band");
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
                    aterm 0.11.0 won\u{2019}t install again.";
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

/// A SECOND LINUX CRASH LOOP IS A ROW OF ITS OWN. A newer
/// build installed over a crash loop's rollback crash-looped too before anything
/// answered the first: the updater restated it, and the window's latch made it a
/// log line, so no row said that a second build had failed. Each crash loop's end
/// ([`aterm_update::INSTALL_CRASH_LOOP_TITLE`]) is said, with its banner, under the
/// install half's title, in the words naming the build that failed — the row up
/// takes them. Negative control: another producer's install warning said again is
/// still the latch's log line.
#[test]
fn a_second_linux_crash_loop_is_a_row_of_its_own() {
    let crash = aterm_update::INSTALL_CRASH_LOOP_TITLE;
    let install = update_words::HealthKind::Install;
    let first = "aterm 0.11.0 didn\u{2019}t start properly, so aterm 0.10.0 is back and aterm \
                 0.11.0 won\u{2019}t install again.";
    let second = "aterm 0.12.0 didn\u{2019}t start properly, so aterm 0.10.0 is back and aterm \
                  0.12.0 won\u{2019}t install again.";
    let words = |app: &App| {
        app.live_update_health(|k| k == install)
            .iter()
            .filter_map(|id| app.messages.live(*id))
            .map(|l| (l.msg.title.clone(), l.msg.detail.join(" ")))
            .collect::<Vec<_>>()
    };
    let mut app = App::headless_for_test();
    band_seen(&mut app);
    assert!(
        app.note_update_health(crash, first),
        "the first is announced"
    );
    assert!(
        app.note_update_health(crash, second),
        "and the second, banner and all"
    );
    let shown = words(&app);
    assert!(
        matches!(shown.as_slice(), [(title, detail)]
            if title == aterm_update::health_failing_title("apply") && detail.contains("0.12.0")),
        "one install warning, in the second's words: {shown:?}"
    );
    let _ = app.settle_messages(
        Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(1),
    );
    assert!(words(&app).is_empty(), "PRECONDITION: it folded");
    assert!(
        !app.note_update_health(aterm_update::health_failing_title("apply"), first),
        "another producer's install warning is still latched"
    );
    assert!(words(&app).is_empty());
    assert!(
        app.note_update_health(crash, second),
        "a crash loop after the fold is a row again"
    );
    assert_eq!(words(&app).len(), 1);
}

/// …SAID WHILE NOBODY COULD SEE IT, THE NOTICE IS SHOWN WHEN A WINDOW CAN, AND
/// ONE WHOSE WAIT ENDED FIRST IS OWED NOTHING (U37's rule, by the row's key).
#[test]
fn a_linux_start_wait_said_unseen_is_shown_once_seen_unless_it_ended() {
    let body = "aterm 0.11.0 was installed over an hour ago and has not started yet: the next \
                aterm you start finishes the update, and update checks wait until then.";
    let secs = std::time::Duration::from_secs;
    let on_screen = |app: &mut App, seen: bool| {
        let ws = app.windows.get_mut(&WindowId(0)).unwrap();
        ws.band_on_screen_for_test = true;
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

/// AN OWED INSTALL WARNING IS ANSWERED BY THE LANDING IT IS CARRIED TO, NEVER
/// SHOWN AGAIN BEFORE IT (the second review of , U37). The
/// overdue update's "Couldn't install updates" was said while no band was on
/// screen and folded unseen; the automatic lane then applied the update. The
/// handoff carried the warning as owed, and the successor posted it on its first
/// settle with a band on screen — before the landing, which is the one proof an
/// install warning needs ([`HealthProof::Landed`]) and which the successor
/// reaches only after its first present (the cold lane) or at Commit (the
/// seamless lanes). The band showed a failure the landing was about to answer,
/// and the history kept a second "Couldn't install updates" beside "aterm
/// updates work again". Both lanes: neither shows the warning nor records it
/// again — the seamless lane carries the parent's row, still waiting unseen
/// there, and the landing answers that one record — and the landing still
/// records its healing against the parent's words.
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
        ws.band_on_screen_for_test = true;
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

    // The cold lane: taken up at construction, settled on every turn before the
    // first present lands the update.
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

    // The seamless lane: seeded under the freeze, landed at Commit.
    parent.post_message(update_words::installing("9.9.9"));
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
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

/// WHAT IS ON SCREEN DECIDES BAND OR RECORD (t-3): a config problem Settings says on
/// the row of the setting it is about ("Not found" on Font family) raised while a
/// terminal was in front is a band row; once Settings comes to the front it becomes
/// its record in place, so the band never repeats the row above it or pushes the
/// page down, and the same words returning never raise it again. NEGATIVE CONTROL: a
/// misspelled key, which no Settings row says, keeps its band row.
#[test]
fn a_font_row_raised_over_a_terminal_is_a_record_once_settings_comes_to_front() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings};
    use aterm_messages::Retired;
    let warnings = || {
        let mut warns = ConfigWarnings::default();
        warns.push(
            ConfigFamily::IgnoredKeys,
            "config line 3: unknown key \"windw_padding\"".to_string(),
        );
        warns.push_shown_in_settings(
            ConfigFamily::Fonts,
            "config font_family: \"NoSuchFontXYZ\" does not resolve to a font file; keeping \
             the current working face"
                .to_string(),
        );
        warns
    };
    let scope: Vec<&'static str> = ConfigFamily::ALL.iter().map(|f| f.key()).collect();
    let reload = |app: &mut App| {
        let warns = warnings();
        app.note_config_settings_shows(&scope, &warns.settings_shown_keys());
        let messages = warns.into_messages_routed(app.settings_in_front());
        app.replace_config_messages(messages);
    };
    let fonts = ConfigFamily::Fonts.key();
    let keys = ConfigFamily::IgnoredKeys.key();
    let mut app = App::headless_for_test();
    assert!(!app.settings_in_front());
    reload(&mut app);
    let row = app
        .messages
        .live_by_key(fonts)
        .map(|live| live.id)
        .expect("over a terminal the problem is a band row");
    assert!(app.messages.live_by_key(keys).is_some());

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
    assert!(app.settings_in_front());
    assert!(
        app.messages.live_by_key(fonts).is_none(),
        "with Settings in front the font problem is no band row"
    );
    let rec = app.messages.log().get(row).expect("logged");
    assert_eq!(rec.retired(), Some(&Retired::Recorded), "{rec:?}");
    assert!(
        app.messages.live_by_key(keys).is_some(),
        "a misspelled key no row says keeps its band row"
    );
    // The same words again with the terminal back in front: still a record.
    app.switch_tab(0);
    assert!(!app.settings_in_front());
    reload(&mut app);
    assert!(
        app.messages.live_by_key(fonts).is_none(),
        "a problem already told is never raised again"
    );
}

/// A REFUSED `font_px` OR THEME IS THE ROW'S TO SAY WHILE SETTINGS IS IN FRONT (t-3's
/// hand-edited `font_px = 500`): the unaccepted-values family Settings says on Font
/// size ("Configured 500 → Default") and Color theme is a record with Settings in
/// front and a band row over a terminal. NEGATIVE CONTROL: a `[harness]` value no row
/// says, in the same family, keeps the band row even with Settings in front.
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
    assert_ne!(msg.hold, Hold::LogOnly, "a band row over a terminal");

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
        "a family with a sentence no row says keeps its band row"
    );
}

/// A BAND ROW THAT BECOMES ITS RECORD DOES NOT SAY ITS FIRST SENTENCE TWICE: a
/// config row raised over a terminal carries its painted excerpt as its first
/// detail line; when Settings comes to the front and the row becomes its record
/// (the decision made on screen, t-3), that excerpt went with it, so Settings ▸
/// Messages read "font_px: 500.0 is not accepted" over "font_px: 500.0 is not
/// accepted (expected 6–200; …)". A record is never painted: it keeps the sentences
/// alone. NEGATIVE CONTROL: the band row keeps its excerpt.
#[test]
fn a_band_row_that_becomes_its_record_says_its_sentence_once() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings, config_record};
    let config = crate::app_config::Config::parse("font_px = 500.0\n").expect("config");
    let mut warns = ConfigWarnings::default();
    crate::app_config::collect_unaccepted_value_notices(
        &mut warns,
        &config,
        &crate::app_config::ThemeCatalog::empty(),
        "font_px = 500.0\n",
    );
    let band = warns
        .into_messages_routed(false)
        .into_iter()
        .find(|m| m.key.as_deref() == Some(ConfigFamily::UnacceptedValues.key()))
        .expect("the refused size is said");
    assert!(
        band.detail.len() >= 2 && band.detail[1].starts_with(&band.detail[0]),
        "the band row paints its excerpt first: {:?}",
        band.detail
    );
    let record = config_record(&band);
    assert_eq!(record.hold, Hold::LogOnly);
    assert_eq!(
        record.detail,
        band.detail[1..].to_vec(),
        "the record keeps the sentences alone"
    );
}

/// THE FIX'S WORDS ARE DECIDED WHEN IT ENDS (appearance-24): a refused wallpaper
/// taken out logs "Wallpaper removed", one that now decodes "Wallpaper loaded", a
/// refused font family taken out "Font setting removed" — never "Font applied" for a
/// font nobody applied — whether the problem was still a band row or already a
/// record. NEGATIVE CONTROL: a reload that saw nothing end keeps the words the family
/// declared at post.
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

    // Through the replace: a live row and a record both close in those words.
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
        // NEGATIVE CONTROL: no observation, the family's own words.
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

/// A RESTATEMENT CARRIES THE TITLE'S SHORT FORM (
/// [`restatement_of`], "every field a builder authored", left it out, and a
/// restatement that changes the title drops the short form it had — so a
/// tab's notice re-worded in place under a new title read, where the whole
/// title does not fit, as a cut of it, never its short form): a row
/// restated to a builder's new words takes their short form with their
/// title, as an ask's step keeps its own short form, and the narrow band
/// paints it. NEGATIVE CONTROL: the old restatement, the short form left
/// unset, leaves the row none, and the band cuts the new whole title.
#[test]
fn a_restatement_carries_the_titles_short_form() {
    use aterm_agent::supervise::policy::exception::Exception;
    use aterm_agent::supervise::policy::notice;
    let spot = crate::message_reporters::UpgradeSpot {
        place: "in tab 1".to_string(),
        nav: Some(aterm_messages::Intent::ShowTab { tab: 1, window: 1 }),
    };
    let ask = |text: &notice::NoticeText| {
        crate::message_reporters::resume_ask(
            "s-1",
            &spot,
            &Exception::of_notice(text, None, None, "x"),
        )
    };
    let first = ask(&notice::LOGIN_SIGN_IN);
    let then = ask(&notice::LIMIT_SIGN_IN);
    assert_ne!(first.title, then.title, "the title changes");
    let short = then.short_title.clone().expect("a short form");
    let restated = |r: Restatement| {
        let now = aterm_messages::Instant::now();
        let mut c = aterm_messages::MessageCenter::new(aterm_messages::MessageLog::empty(), now);
        let id = c
            .post(first.clone(), aterm_messages::WallStamp { unix_ms: 1 }, now)
            .id;
        assert!(c.restate(id, r, now), "the row is live");
        let kept = c.live(id).and_then(|l| l.msg.short_title.clone());
        let later = now + aterm_messages::PROGRESS_GRACE + aterm_messages::Duration::from_millis(1);
        let _ = c.settle(later, true);
        c.commit_rows(later, 3);
        // A band too narrow for the whole title beside its capsules.
        let mut p = c.presentation(
            40,
            &crate::message_band::cell_width,
            None,
            aterm_messages::Links::Painted,
        );
        assert_eq!(p.rows.len(), 1, "one row on the band");
        (kept, p.rows.remove(0).title.1)
    };
    let (kept, shown) = restated(restatement_of(&then));
    assert_eq!(kept.as_deref(), Some(short.as_str()));
    assert_eq!(shown, short, "the band paints the new short form");
    // NEGATIVE CONTROL: the old restatement.
    let (kept, shown) = restated(Restatement {
        short_title: None,
        ..restatement_of(&then)
    });
    assert_eq!(kept, None, "the title's change drops the short form");
    assert!(
        shown.ends_with('\u{2026}') && then.title.starts_with(shown.trim_end_matches('\u{2026}')),
        "the control: a cut of the whole title: {shown:?}"
    );
}

/// A TOOL'S `notice later … url=` (codex-alab's account security setup, said by every tab
/// of every launch) is one entry of the digest, never a band row; its one button, `Open
/// in browser`, files it answered whatever the browser does (a headless instance opens
/// none and says so), and that answer holds against the next tab's post.
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
    assert_eq!(app.messages.wanted_rows(), 0, "never a band row");
    let digest = app.messages.digest();
    assert_eq!(digest.len(), 1);
    let entry = &digest.entries[0];
    assert_eq!(
        entry.actions,
        [Intent::OpenLink {
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
    // Establish the already-filed baseline with a one-minute writer hang detector;
    // the earlier press may have exhausted its normal UI wait. The Done readback
    // below gets no manual flush, so only its reply path can persist that reset.
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
    // The ended cause is written before the acknowledgement, although the open
    // digest was empty throughout this request. No manual flush papers over it.
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
    drop(app.messages_log.take()); // Join the writer before the scratch directory goes.
}

/// `Open in browser` opens only a web page on the host its entry named: the engine's
/// rule and the click gate, both, at the press, whatever road the intent came by.
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
