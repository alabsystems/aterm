// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::update_words::LiveHealth as _;
#[cfg(unix)]
use aterm_messages::HOLD_SUCCESS;
use aterm_messages::{
    HOLD_GESTURE, LogRecord, LogState, MAX_ROWS, Meter, Retired, Severity, tags, words,
};

fn warn(title: &str) -> Message {
    Message::new(tags::CONFIG, Severity::Warn, title).line("why")
}

use crate::toolchain_words::held_row;

/// A settle this far past a resolve commits the band's quiet shrink:
/// [`aterm_messages::SHRINK_QUIET`], or the longest echo (a Complete from
/// an empty fill) when that is later — a quiet shrink waits for every echo
/// to end (design ruling 415 (b)) — plus a millisecond.
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
/// settle it arms, both reading the one predicate.
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
    // A LIVE row's staleness cap still wakes the loop mid-freeze.
    let live = app.post_message(
        Message::new(tags::UPDATE, Severity::Info, "downloading").hold(Hold::Live {
            stale_after: std::time::Duration::from_secs(30),
        }),
    );
    assert!(app.messages.live(live).is_some());
    assert!(app.messages_deadline().is_some(), "staleness keeps running");
    app.incoming_handoff_pending = false;
    assert!(!app.message_holds_frozen());
}

/// A shrink pending when a handoff freezes the band is not a wake: only
/// `commit_rows` takes it, and the freeze skips `commit_rows`, so an
/// armed shrink would come due and stay due for the whole park → proof
/// → exec window (the audit's stale-arm finding).
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

/// Mid-handoff the row count never moves — the successor's layout must
/// match the parent's capture — and `sync_messages` still runs the rest.
#[test]
fn a_handoff_freeze_settles_only_staleness_and_never_regrids() {
    let mut app = App::headless_for_test();
    app.incoming_handoff_pending = true;
    let id = app.post_message(warn("frozen"));
    assert!(app.messages.live(id).is_some(), "the row is live");
    assert_eq!(app.message_band_rows, 0, "…but no row was committed");
    assert!(
        !app.sync_message_band_rows(Instant::now()),
        "and none will be, mid-handoff"
    );
    app.incoming_handoff_pending = false;
    assert!(
        app.sync_message_band_rows(Instant::now()),
        "the freeze lifted: the row commits"
    );
    assert_eq!(app.message_band_rows, 1);
}

/// THE SEAMLESS LANDING NAMES THE REPAINTED TABS (the 2026-09-22/23
/// update audit, plan P1-5), on main's messages surfaces: the count the
/// boot took from the adopted set reaches the landing RECORD the Commit
/// arm writes (ruling 141) — and only the record: one fact is one log
/// entry, and a no-action row on the glass after the landing is what
/// ruling 141 made record-only. A launch that adopted nothing counts
/// nothing.
#[test]
fn the_seamless_landing_names_the_tabs_the_successor_repainted() {
    let mut app = App::headless_for_test();
    assert_eq!(
        app.seamless_repainted_tabs(),
        0,
        "a launch that adopted nothing counts nothing"
    );
    app.post_update_landed("0.92.0", 7, app.seamless_repainted_tabs());
    assert!(
        app.messages.live_rows().next().is_none(),
        "an exact landing is a record only"
    );
    app.handoff_repainted_tabs = 2;
    let entries = app.messages.log().records().count();
    app.post_update_landed("0.92.0", 7, app.seamless_repainted_tabs());
    assert!(
        app.messages.live_rows().next().is_none(),
        "a repainted landing is a record only too"
    );
    assert!(
        app.messages
            .log()
            .records()
            .any(|r| r.title == "Updated to aterm v0.92.0"
                && r.detail.first().map(String::as_str) == Some("2 tabs repainted")),
        "the landing record names the repaint"
    );
    assert_eq!(
        app.messages.log().records().count(),
        entries + 1,
        "one fact, one log entry"
    );
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
    app.drain_message_inbox();
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "an empty inbox posts nothing"
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

/// AUDIT #7 FINDING 48 — the folders fresh shells could not start in fold
/// into ONE row per fault at the park: a restore that meets two gone
/// folders is one row naming both, a New Tab from a third while that row
/// is up joins it, a folder that could not be entered is its own row, and
/// once the person has dismissed a row the next folder is a fresh row
/// naming only itself.
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
    assert_eq!(live.msg.title, "Couldn't find 2 folders");
    assert_eq!(
        live.msg.detail,
        ["opened in your home folder instead", "/work/a", "/work/b"]
    );

    fell_back("/work/c", Fault::Missing);
    fell_back("/srv/locked", Fault::Shut);
    app.drain_message_inbox();
    assert_eq!(rows(&app, key), 1, "still one row");
    let live = app.messages.live_by_key(key).expect("the row");
    assert_eq!(live.msg.title, "Couldn't find 3 folders");
    assert_eq!(&live.msg.detail[1..], ["/work/a", "/work/b", "/work/c"]);
    let shut = crate::message_reporters::KEY_FOLDER_NOT_OPENED;
    assert_eq!(rows(&app, shut), 1, "a folder not entered: its own row");
    let live = app.messages.live_by_key(shut).expect("the row");
    assert_eq!(live.msg.title, "Couldn't open the folder");

    let id = app.messages.live_by_key(key).expect("the row").id;
    assert!(app.messages.dismiss(id, Instant::now()));
    fell_back("/work/d", Fault::Missing);
    app.drain_message_inbox();
    assert_eq!(
        app.messages
            .live_by_key(key)
            .expect("a fresh row")
            .msg
            .detail,
        ["opened in your home folder instead of /work/d"]
    );
}

/// A ROW CARRIED ACROSS AN UPDATE KEEPS ITS FOLDERS: the successor's list
/// starts empty while the carried row is up, and the next folder joins
/// the folders that row named instead of replacing them. The negative
/// control is the same folder with no row up: a fresh row naming it alone.
#[test]
fn a_carried_folder_row_keeps_its_folders_when_the_next_one_joins() {
    use crate::spawn_folder::{Fault, Named};
    let _lane = crate::message_inbox::lane_test_guard();
    let _ = crate::message_inbox::take_queued();
    let _ = crate::spawn_folder::take_noted();
    let key = crate::message_reporters::KEY_FOLDER_NOT_FOUND;

    let mut fresh = App::headless_for_test();
    fell_back("/work/c", Fault::Missing);
    fresh.drain_message_inbox();
    let live = fresh.messages.live_by_key(key).expect("the row");
    assert_eq!(live.msg.title, "Couldn't find the folder", "no row was up");

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
    assert_eq!(live.msg.title, "Couldn't find 3 folders");
    assert_eq!(&live.msg.detail[1..], ["/work/a", "/work/b", "/work/c"]);
}

/// A reload's set REPLACES the config rows: the old family rows are
/// resolved (the log says so), the new ones posted, in one settle; an
/// EMPTY set — the fixed-typo reload — takes every `config.` row down and
/// touches nothing keyed elsewhere.
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

    // The reload fixed the chord and left the typo: the chord's row is
    // resolved, and the typo's — the same words — STANDS, untouched: no
    // resolve, no second post, no second announcement (review
    // 2026-09-24).
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

    // The fixed reload: nothing to post, every config row down.
    app.replace_config_messages(Vec::new());
    assert!(app.messages.live_by_key("config.ignored-keys").is_none());
    assert!(app.messages.live(other).is_some());
    assert_eq!(app.messages.live_rows().count(), 1);
    assert_eq!(
        app.messages.wanted_rows(),
        1,
        "the band wants the survivor's row only (the committed count follows after the shrink quiet)"
    );
}

/// A REPEAT GOES TO THE LOG (review 2026-09-24). A reload runs on every
/// Settings patch, Presence toggle and Serious Mode write, so once the
/// `Unknown key` row has left the glass — its hold ran out, or it was
/// read — the next unrelated toggle must not put the same words back and
/// announce them again: they are a RECORD. A CHANGED problem (a new typo)
/// raises its row; a FIXED one that comes back later is news again.
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
    // A CHANGED problem is news: its row is raised.
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
    // …and when the row had ALREADY left the glass when the fix landed
    // (read, then fixed, then broken the same way): news again too.
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
    // …and its next repeat is a record again.
    let raised = app.messages.live_by_key(key).map(|l| l.id).unwrap();
    assert!(app.messages.mark_seen(raised, Instant::now()));
    app.sync_messages();
    app.replace_config_messages(typo(5, "colums"));
    assert!(app.messages.live_by_key(key).is_none());
}

/// The CONTENT-EQUAL reload re-derives only the two text-read families,
/// so its replace resolves only those (and the launch load failure a
/// file that now parses has answered): a still-true keybinding row
/// survives a comment-only save, where the prefix resolve took it down
/// and logged it `resolved` for a config that was still broken
/// (ruling 28).
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

    // A new typo on the same path: its family's row is posted, the
    // chord's row still stands, and a key with no live row resolves
    // nothing.
    let mut reload = ConfigWarnings::default();
    reload.push(
        ConfigFamily::IgnoredKeys,
        "config line 5: unknown key \"colums\"".into(),
    );
    app.replace_config_messages_keyed(&keys, reload.into_messages());
    assert!(app.messages.live(chord).is_some());
    assert!(
        app.messages
            .live_by_key(ConfigFamily::IgnoredKeys.key())
            .is_some_and(|l| l.msg.detail.iter().any(|d| d.contains("colums")))
    );
    assert_eq!(app.messages.live_rows().count(), 2);
}

/// A CHANGED PROBLEM IS NOT A FIXED ONE (design ruling 266). Two bad
/// chords with one fixed, or one typo joined by a second, change the
/// family's words while it is still broken: the old row is SUPERSEDED in
/// place, and its record keeps its title and its ⚠ — never `Keybinding
/// fixed` / `Misspelled setting fixed` under a ✓ that left Problems. Only
/// the reload that takes the family away logs the fix's words.
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
    assert_eq!(rec.title, "2 keybindings skipped", "{rec:?}");
    assert_eq!(rec.severity, Severity::Warn, "still in Problems: {rec:?}");
    assert_eq!(
        app.messages.wanted_rows(),
        1,
        "one row, in the old one's place"
    );

    // The last chord fixed: the family is gone, and now it IS fixed.
    app.replace_config_messages(Vec::new());
    assert!(app.messages.live_by_key(key).is_none());
    let rec = app.messages.log().get(one).expect("logged");
    assert_eq!(rec.retired(), Some(&Retired::Resolved(Outcome::Ok)));
    assert_eq!(rec.title, "Keybinding fixed", "{rec:?}");
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
    assert!(
        !rec.title.ends_with("fixed") && rec.severity == Severity::Warn,
        "a typo still there is not logged fixed: {rec:?}"
    );
    assert!(app.messages.live_by_key(key).is_some());
}

/// A RETIRED SPELLING IS A RECORD ON EVERY PATH (design ruling 42): the
/// launch's and every reload's `collect_key_notices` puts
/// `[packages] auto_update = false` in `ConfigFamily::RetiredKeys`, and the
/// replace logs it without a row, so a comment-only save never puts it back
/// on glass. The typo beside it keeps its row.
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
    // Design §10.3: the title names the kind in a few words; the key's
    // own sentence is the detail.
    assert_eq!(records[0].title, "Retired setting");
    assert!(
        records[0]
            .detail
            .iter()
            .any(|d| d.contains("auto_update = false is retired")),
        "{:?}",
        records[0].detail
    );
}

/// Restate and resolve go through the center and sync; a restatement of a
/// builder's words carries every field.
/// THE FINAL READ LEAVES A HELD ROW ALONE (ruling 19). Since the silent
/// lane the `installed` words are a record, so a HELD row on the pass key
/// is one carried across a handoff from a build before it — stood in for
/// here by the record's words at the default hold. The tailer's final
/// snapshot of the ended pass changes nothing on it, first run or not: no
/// meter, no words, no revision, no log line.
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
    let live = app.messages.live(id).expect("still on glass, still held");
    assert_eq!(live.revision, 0, "not restated");
    assert_eq!(live.msg.meter, None, "no meter, no 100%");
    assert_eq!(live.msg.title, "ALab tools installed");
    assert_eq!(live.msg.detail, vec!["claude, codex"]);
    assert_eq!(app.messages.log().len(), 1, "the read posted nothing");
}

#[test]
fn restate_and_resolve_reach_the_center() {
    let mut app = App::headless_for_test();
    let id = app.post_message(
        Message::new(tags::TOOLCHAIN, Severity::Info, "Installing").hold(Hold::Live {
            stale_after: std::time::Duration::from_secs(30),
        }),
    );
    let words = Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
        .line("trust · extracting")
        .meter(Meter {
            fill_permille: Some(420),
            stats: "1 of 2".into(),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: std::time::Duration::from_secs(30),
        });
    let r = restatement_of(&words);
    assert_eq!(r.title.as_deref(), Some("Installing"));
    assert_eq!(r.detail, Some(vec!["trust · extracting".to_string()]));
    assert_eq!(r.meter, Some(words.meter.clone()));
    assert_eq!(r.actions, Some(Vec::new()));
    assert_eq!(r.severity, Some(Severity::Info));
    assert!(app.restate_message(id, r));
    assert_eq!(app.messages.live(id).unwrap().revision, 1);
    assert_eq!(app.messages.log().len(), 1, "a restate writes no log line");
    assert!(!app.restate_message(MessageId::from_raw(9_999).unwrap(), Restatement::default()));
    assert!(app.resolve_message(id, Outcome::Ok));
    assert!(app.messages.live(id).is_none());
    assert_eq!(
        app.messages.log().get(id).unwrap().state,
        LogState::Retired(Retired::Resolved(Outcome::Ok))
    );
    assert!(!app.resolve_message(id, Outcome::Ok), "already gone");
}

/// `Show tab N` from ANOTHER window's band (ruling 247): the band is
/// app-wide, so every window's band shows the strain row and its capsule.
/// The press lands on the window the row named — its tab 2 selected —
/// and never switches the pressing window to ITS tab 2, a different
/// program. A gone tab or window is an honest miss.
#[test]
fn show_tab_goes_to_the_named_window_not_the_pressing_one() {
    let mut app = App::headless_for_test();
    let a = WindowId(0);
    app.push_stub_tab(a, crate::stub_session(app.next_session_id));
    app.switch_tab_in(a, 0);
    let b = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
    app.push_stub_tab(b, crate::stub_session(app.next_session_id));
    app.switch_tab_in(b, 0);
    let active = |app: &App, w: WindowId| app.windows[&w].tab_set.active_index();
    assert_eq!((active(&app, a), active(&app, b)), (Some(0), Some(0)));
    let id = app.post_message(warn("Typing slowed by 'yes' (tab 2)"));
    let show = Intent::ShowTab {
        tab: 2,
        window: a.0,
    };
    assert!(app.perform_intent(b, id, show));
    assert_eq!(active(&app, a), Some(1), "the named window shows tab 2");
    assert_eq!(active(&app, b), Some(0), "the pressing window is untouched");
    let gone = app.post_message(warn("gone"));
    assert!(!app.perform_intent(
        b,
        gone,
        Intent::ShowTab {
            tab: 3,
            window: a.0,
        }
    ));
    let gone = app.post_message(warn("gone window"));
    assert!(!app.perform_intent(a, gone, Intent::ShowTab { tab: 1, window: 99 }));
}

/// THE DETAILS PRESS (design §3.6): the row is seen — a held row folds,
/// the config band's "click at a notice you have read" — and Settings ▸
/// Messages opens in THE PRESSED WINDOW with that entry selected, the
/// page already showing the record as folded. A standing row stays and
/// is selected all the same; the overflow link opens the page unselected.
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
    assert!(
        app.messages_last_publish.is_some(),
        "the page was published on the way in"
    );
    assert_eq!(
        app.messages_published_revision,
        app.messages.revision(),
        "…at the center's revision AFTER the fold, so the entry reads as folded"
    );
    // A STANDING row folds once read too (design §10.4.3 E3, ruling 79).
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
    // A LIVE row stays — work in flight is not read away — and is selected.
    let live = app.post_message(
        Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.92.0")
            .in_flight()
            .hold(Hold::Live {
                stale_after: aterm_messages::STALE_UPDATE,
            }),
    );
    assert!(app.perform_intent(wid, live, Intent::Details));
    assert!(app.messages.live(live).expect("a live row stays").seen);
    assert_eq!(
        settings_view(&app, wid).unwrap().messages_selected,
        Some(live.raw())
    );
    // The overflow row's link: the page, top of the log, nothing selected.
    assert!(app.open_messages_entry(wid, None));
    let view = settings_view(&app, wid).unwrap();
    assert_eq!(view.route, SettingsRoute::Messages);
    assert_eq!(
        view.messages_selected,
        Some(live.raw()),
        "a plain open leaves the page as it was"
    );
    // A window that does not exist opens nothing.
    assert!(!app.open_messages_entry(WindowId(77), Some(live)));
}

/// A "TO LOOK AT" LINE IN THE MENU lands on ITS entry: the page already
/// open on another selection moves to the pressed one (a plain route
/// change left it where it was), and with no frontmost window the first
/// window shows it rather than the press doing nothing.
#[test]
fn a_menu_line_opens_messages_at_its_entry() {
    let mut app = App::headless_for_test();
    let wid = *app.windows.keys().next().expect("a window");
    app.frontmost_window = None;
    let first = app.post_message(warn("first"));
    let second = app.post_message(warn("second"));
    assert!(app.open_messages_entry(wid, Some(first)));
    assert!(app.open_messages_entry_anywhere(second.raw()));
    let view = settings_view(&app, wid).expect("Settings in the first window");
    assert_eq!(view.route, SettingsRoute::Messages);
    assert_eq!(view.messages_selected, Some(second.raw()));
}

/// ROUND 16, DAY TWO (ruling 267): a row still open when aterm quits is
/// recorded as cut off by the quit — its words, its mark, the percent a
/// determinate row reached — never `stopped reporting`, which blamed a
/// reporter that had done nothing wrong; and a record replayed open from a
/// process that died without an ending says aterm stopped, while a row
/// THIS process retired for going silent still blames its reporter.
#[test]
fn a_row_open_at_quit_is_recorded_as_cut_off_and_a_dead_processes_as_open() {
    let mut app = App::headless_for_test();
    let render = app.post_message(
        Message::new(tags::SYSTEM, Severity::Info, "Rendering the video")
            .glyph(aterm_messages::Glyph::or_fallback('\u{21bb}'))
            .hold(Hold::Live {
                stale_after: aterm_messages::STALE_TAILED,
            })
            .meter(Meter {
                fill_permille: Some(283),
                ..Meter::default()
            }),
    );
    let warn = app.post_message(
        Message::new(tags::SYSTEM, Severity::Warn, "Disk nearly full")
            .line("2 GB free on the startup disk"),
    );
    assert_eq!(app.close_messages_at_exit(), 2);
    assert_eq!(app.messages.live_rows().count(), 0);
    let state = app.messages_state();
    let r = state.entry(render.raw()).expect("the render's record");
    assert_eq!(r.state, "quit");
    assert_eq!(r.title, "Rendering the video");
    assert_eq!(r.detail, vec!["28% when aterm quit".to_string()]);
    // Ruling 317 (day eight E5): work cut off no longer wears the working
    // `↻`; its severity's mark stands in.
    assert_eq!(r.glyph, '\u{2139}', "the quit record's mark");
    assert!(
        r.state_words().starts_with("still open when aterm quit"),
        "{}",
        r.state_words()
    );
    let w = state.entry(warn.raw()).expect("the warning's record");
    assert_eq!(w.state, "quit");
    assert!(w.alarm(), "the warning stays in Problems");
    // A dead process's open row, replayed: no retirement time.
    let dead = LogRecord::from_posted(
        MessageId::from_raw(900).expect("an id"),
        WallStamp { unix_ms: 1_000 },
        &Message::new(tags::SYSTEM, Severity::Info, "Rendering the video")
            .glyph(aterm_messages::Glyph::or_fallback('\u{21bb}')),
    );
    let mut log = aterm_messages::MessageLog::empty();
    log.replay(aterm_messages::LogLine::Posted(dead));
    let replayed = log.records().next().expect("replayed");
    let view = MessageView::of(replayed, None, None, None, &PageDesk::of(&app));
    assert_eq!(view.state, "stale");
    assert_eq!(view.state_words(), "still open when aterm stopped");
    assert_eq!(view.glyph, '\u{2139}', "the replayed open record's mark");
    // Control: the same record while live keeps its working mark, and a
    // mark that is not a working one is kept when cut off — the mark's
    // own law, moved with it to the engine (design ruling 382):
    // `aterm_messages::page_tests::a_replayed_open_record_is_cut_off_and_a_silent_one_blames_its_reporter`.
    // A row this process retired Stale keeps its words.
    let mut silent = replayed.clone();
    silent.retired_unix_ms = Some(61_000);
    assert_eq!(
        MessageView::of(&silent, None, None, None, &PageDesk::of(&app)).state_words(),
        "stopped reporting after 1 min"
    );
}

/// THE PROJECTION (design §4.2): every record newest first, in the
/// wire's own state words, the live row's current words, the band row
/// it is on, tag counts in the chips' order — and `still_actionable`
/// following the intent and the row's life: a decision only while live,
/// `Apply now` only while that build is staged, `Open log` only while
/// the file exists, a navigation always.
#[test]
fn the_projection_is_newest_first_in_the_wires_words_with_live_actionability() {
    let mut app = App::headless_for_test();
    let first = app.post_message(
        Message::new(
            tags::CRASH,
            Severity::Error,
            "aterm closed unexpectedly last time",
        )
        .line("crash log at /definitely/not/there.log")
        .action(Intent::OpenPath {
            path: "/definitely/not/there.log".into(),
        }),
    );
    let ask = app.post_message(
        Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
            .action(Intent::OpenSystemPane {
                pane: "full-disk-access".into(),
            })
            .action(Intent::NotNow {
                decision: Decision::FileAccess,
            })
            .hold(Hold::asking(aterm_messages::HOLD_ASK)),
    );
    let staged = app.post_message(update_words::staged(
        "0.91.0",
        1234,
        Some(update_words::ApplyPosture::ManualByConfig),
    ));
    let meter = app.post_message(
        Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
            .line("trust \u{00b7} extracting")
            .action(Intent::OpenSettings {
                route: "/packages".into(),
            })
            .hold(Hold::Live {
                stale_after: aterm_messages::STALE_TAILED,
            }),
    );
    assert!(app.messages.restate(
        meter,
        Restatement {
            title: Some("Installing ALab tools".into()),
            detail: Some(vec!["trust \u{00b7} linking".into()]),
            ..Restatement::default()
        },
        Instant::now()
    ));
    assert!(app.resolve_message(first, Outcome::Warn));

    let state = app.messages_state();
    assert_eq!(state.revision, app.messages.revision());
    let ids: Vec<u64> = state.entries.iter().map(|e| e.id).collect();
    assert_eq!(
        ids,
        [meter.raw(), staged.raw(), ask.raw(), first.raw()],
        "newest first"
    );
    assert_eq!(
        state.tags,
        [
            ("crash".to_string(), 1),
            ("update".to_string(), 1),
            ("toolchain".to_string(), 1),
            ("privacy".to_string(), 1),
        ],
        "the chips' fixed order, present tags only"
    );
    let entry = |id: MessageId| state.entry(id.raw()).expect("in the projection");
    // The live meter: its CURRENT words (the restatement is never logged).
    let m = entry(meter);
    assert_eq!(m.state, "live");
    assert_eq!(m.detail, vec!["trust \u{00b7} linking"]);
    assert_eq!(m.tag, "toolchain");
    assert_eq!(m.severity, "info");
    assert!(m.actions[0].still_actionable, "a navigation is always live");
    assert_eq!(m.actions[0].label, "Packages");
    assert_eq!(m.state_words(), "showing now");
    assert_eq!(m.severity_words(), "Info");
    assert_eq!(tag_words(&m.tag), "ALab tools");
    // The ask: held (a question waits on its patience), both decisions
    // pressable while it is up.
    let a = entry(ask);
    assert_eq!(a.state, "held");
    assert!(a.actions.iter().all(|x| x.still_actionable));
    assert_eq!(
        a.actions.iter().map(|x| x.label).collect::<Vec<_>>(),
        ["Open Settings", "Not now"]
    );
    // The staged row: `Apply now` only while that build is really staged
    // (a test App stages nothing), the page link always.
    let s = entry(staged);
    assert_eq!(s.state, "held");
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
    // The crash record, retired: the wire's word, the retired stamp, and
    // an `Open log` whose file is gone.
    let f = entry(first);
    assert_eq!(f.state, "resolved-warn");
    assert!(f.retired_unix_ms.is_some());
    assert!(
        f.state_words().starts_with("ended with a problem after "),
        "{}",
        f.state_words()
    );
    // A delivered row says so; neither outcome reads "cleared" (audit
    // 2026-09-24).
    let delivered = MessageView {
        state: "resolved-ok",
        ..f.clone()
    };
    assert!(
        delivered.state_words().starts_with("lasted "),
        "{}",
        delivered.state_words()
    );
    // Delivered work (its record's mark is ✓ Success, ruling 265) took
    // its span; a plain withdraw lasted it — never `withdrawn`.
    let took = MessageView {
        state: "withdrawn",
        severity: "success",
        ..f.clone()
    };
    assert!(
        took.state_words().starts_with("took "),
        "{}",
        took.state_words()
    );
    let ended = MessageView {
        state: "withdrawn",
        severity: "info",
        ..f.clone()
    };
    assert!(
        ended.state_words().starts_with("lasted "),
        "{}",
        ended.state_words()
    );
    assert!(!f.actions[0].still_actionable, "no file, no press");
    assert_eq!(f.glyph, '\u{2715}');
    assert_eq!(f.repeats, 1);
    // What the page prints is what the wire prints.
    let rec = app.messages.log().get(first).unwrap();
    assert_eq!(f.state, wire::state_word(rec, None));
    assert_eq!(f.copy_text(), words::copy_text(rec));
    // Every vocabulary word has a chip position.
    for tag in tags::ALL {
        assert!(TAG_ORDER.contains(&tag.as_str()), "{tag}");
    }
    assert_eq!(TAG_ORDER.len(), tags::ALL.len());
    // A decision row answered: its label in the meta words.
    let answered = app.post_message(
        Message::new(tags::PRIVACY, Severity::Info, "Allow Full Disk Access?")
            .action(Intent::NotNow {
                decision: Decision::FileAccess,
            })
            .hold(Hold::asking(aterm_messages::HOLD_ASK)),
    );
    assert_eq!(
        app.messages.act(answered, ActionIndex(0), Instant::now()),
        Some(Intent::NotNow {
            decision: Decision::FileAccess
        })
    );
    let state = app.messages_state();
    let ans = state.entry(answered.raw()).unwrap();
    assert_eq!(ans.state, "answered");
    assert_eq!(ans.state_words(), "answered: Not now");
    assert!(
        ans.actions.is_empty(),
        "a decision is answered once, and not offered again (ruling 265): {:?}",
        ans.actions
    );
    // A script's tags read like the chips beside them (ruling 265).
    assert_eq!(tag_words("script"), "Scripts");
    assert_eq!(tag_words("search"), "Search");
    assert_eq!(tag_words("ci"), "CI");
    assert_eq!(tag_words("Deploy"), "Deploy");
    assert_eq!(tag_words("crash"), "Crashes");
    // The span words are the engine's own law, moved with it (design
    // ruling 382): `aterm_messages::page_tests::tags_chips_cover_the_vocabulary`.
}

/// THE PAGE'S PRESS (`perform_message_act`): a live row's button goes
/// through the engine's `act` — logged, a `Not now` retiring its row —
/// then the intent is performed in the pressing window and worded; a
/// retired record re-offers its navigation from the log; a decision
/// cannot be answered once its row is gone; an unknown id or button is
/// `Gone`.
#[test]
fn the_pages_press_acts_through_the_engine_and_words_the_outcome() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let ask = app.post_message(
        Message::new(tags::PRIVACY, Severity::Info, "Allow Full Disk Access?")
            .action(Intent::NotNow {
                decision: Decision::FileAccess,
            })
            .hold(Hold::asking(aterm_messages::HOLD_ASK)),
    );
    assert_eq!(
        app.perform_message_act(wid, ask.raw(), 0),
        MessageActOutcome::Performed {
            feedback: "Not now recorded".to_string()
        }
    );
    assert!(
        app.messages.live(ask).is_none(),
        "the answer retired the row"
    );
    assert_eq!(
        app.messages
            .log()
            .get(ask)
            .unwrap()
            .last_action
            .as_ref()
            .map(|(l, _)| l.as_str()),
        Some("Not now"),
        "the press is on record"
    );
    assert_eq!(
        app.perform_message_act(wid, ask.raw(), 0),
        MessageActOutcome::Gone,
        "a decision cannot be answered twice"
    );
    // A retired navigation, re-offered from the log.
    let nav = app.post_message(
        Message::new(tags::TOOLCHAIN, Severity::Success, "ALab tools installed").action(
            Intent::OpenSettings {
                route: "/packages".into(),
            },
        ),
    );
    assert!(app.resolve_message(nav, Outcome::Ok));
    assert_eq!(
        app.perform_message_act(wid, nav.raw(), 0),
        MessageActOutcome::Performed {
            feedback: "Opened Settings \u{25b8} Packages".to_string()
        }
    );
    assert_eq!(
        settings_view(&app, wid).map(|v| v.route),
        Some(SettingsRoute::Packages)
    );
    assert_eq!(
        app.perform_message_act(wid, nav.raw(), 1),
        MessageActOutcome::Gone
    );
    assert_eq!(app.perform_message_act(wid, 0, 0), MessageActOutcome::Gone);
    assert_eq!(
        app.perform_message_act(wid, 9_999, 0),
        MessageActOutcome::Gone
    );
    // A refusal is worded as one.
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
    assert_eq!(
        app.perform_message_act(wid, crash.raw(), 0),
        MessageActOutcome::Refused {
            feedback: "Could not open the log".to_string()
        }
    );
    assert_eq!(
        act_feedback(&Intent::NewWindow, true, false),
        "Opened a new window"
    );
    // An Install now press says what it did: installs only where it
    // applies, the page it opened otherwise, and a failure as one.
    let apply = Intent::ApplyUpdate { build: 1 };
    assert_eq!(act_feedback(&apply, true, true), "Installing the update");
    assert_eq!(
        act_feedback(&apply, true, false),
        "Opened Settings \u{25b8} Software Update"
    );
    assert_eq!(
        act_feedback(&apply, false, true),
        "Could not start the install"
    );
    assert_eq!(
        act_feedback(&apply, false, false),
        "Could not open Settings"
    );
    assert_eq!(
        act_feedback(
            &Intent::OpenSettings {
                route: "/nowhere".into()
            },
            false,
            false
        ),
        "Could not open Settings",
        "no route string on the page"
    );
}

/// THE PUBLISH GATE (design §4.2): nothing is published without a
/// Settings instance; with one, the first sync publishes, the next
/// within the 2 Hz gap waits (and arms the deadline the park serves),
/// a quiet center republishes only on the 60 s tick while a view is on
/// the route, and opening Settings publishes unconditionally.
#[test]
fn the_projection_is_published_at_two_hertz_and_on_the_minute_tick() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    app.post_message(warn("before Settings"));
    assert_eq!(app.messages_last_publish, None, "no Settings, no publish");
    assert_eq!(app.messages_publish_due_at(), None);
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Home));
    let opened = app
        .messages_last_publish
        .expect("opening Settings publishes");
    assert_eq!(app.messages_published_revision, app.messages.revision());
    assert_eq!(app.messages_publish_due_at(), None, "nothing pending");
    // A post inside the gap: the center moved, the publish waits, and
    // the deadline says when.
    app.post_message(warn("inside the gap"));
    assert_eq!(
        app.messages_last_publish,
        Some(opened),
        "within 500 ms of the last publish nothing is republished"
    );
    assert_ne!(app.messages_published_revision, app.messages.revision());
    assert_eq!(
        app.messages_publish_due_at(),
        Some(opened + crate::app_native::MESSAGES_PUBLISH_MIN_GAP)
    );
    assert!(
        app.messages_deadline()
            .is_some_and(|d| d <= opened + crate::app_native::MESSAGES_PUBLISH_MIN_GAP),
        "the loop is asked back for the pending publish"
    );
    // The park, past the gap: published.
    app.publish_native_messages_state_if_due(opened + crate::app_native::MESSAGES_PUBLISH_MIN_GAP);
    assert_eq!(app.messages_published_revision, app.messages.revision());
    let second = app.messages_last_publish.unwrap();
    assert_ne!(second, opened);
    // A quiet center off the route: the minute tick publishes nothing.
    app.publish_native_messages_state_if_due(second + crate::app_native::MESSAGES_PUBLISH_TICK);
    assert_eq!(app.messages_last_publish, Some(second));
    // On the route: it does, so the relative times move.
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    let on_route = app.messages_last_publish.unwrap();
    app.publish_native_messages_state_if_due(
        on_route + crate::app_native::MESSAGES_PUBLISH_TICK - std::time::Duration::from_secs(1),
    );
    assert_eq!(app.messages_last_publish, Some(on_route), "not yet");
    app.publish_native_messages_state_if_due(on_route + crate::app_native::MESSAGES_PUBLISH_TICK);
    assert_ne!(app.messages_last_publish, Some(on_route), "the tick");
}

/// ROUND 16 REVIEW (ruling 267): the page's status line (`Copied`) is
/// timed by the projections, and on a quiet page those came once a
/// minute — so it stood one to two minutes, never about five seconds. The
/// host now arms the publish that starts its clock and, five seconds on,
/// the one that clears it; then only the minute tick again. Off screen it
/// arms nothing.
#[test]
fn a_transient_status_line_is_cleared_about_five_seconds_on() {
    use crate::app_native::{
        MESSAGES_FEEDBACK_WAIT as WAIT, MESSAGES_PUBLISH_MIN_GAP as GAP,
        MESSAGES_PUBLISH_TICK as TICK,
    };
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
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
    view_mut(&mut app, wid).show_messages_feedback_for_test("Copied");
    // Off screen: nothing armed but the (gated) tick — nothing at all.
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = false;
    assert_eq!(
        app.messages_publish_due_at(),
        None,
        "a hidden page arms nothing"
    );
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    // The publish that starts the clock is due at the 2 Hz gap.
    assert_eq!(app.messages_publish_due_at(), Some(opened + GAP));
    app.publish_native_messages_state_if_due(opened + GAP);
    let stamped = app.messages_last_publish.unwrap();
    assert_ne!(stamped, opened);
    assert_eq!(
        settings_view(&app, wid).unwrap().messages_feedback_clock(),
        Some(true)
    );
    assert_eq!(
        settings_view(&app, wid).unwrap().feedback.as_deref(),
        Some("Copied")
    );
    // The one that clears it, five seconds on — not the minute tick.
    assert_eq!(app.messages_publish_due_at(), Some(stamped + WAIT));
    assert!(app.messages_deadline().is_some_and(|d| d <= stamped + WAIT));
    app.publish_native_messages_state_if_due(stamped + WAIT - std::time::Duration::from_millis(1));
    assert_eq!(app.messages_last_publish, Some(stamped), "not yet");
    // The page times the line by the projections' wall clock, which a test
    // cannot wait out: age the stamp as five seconds would.
    view_mut(&mut app, wid)
        .age_messages_feedback_for_test(crate::native_settings::MESSAGES_FEEDBACK_MS);
    app.publish_native_messages_state_if_due(stamped + WAIT);
    let cleared = app.messages_last_publish.unwrap();
    assert_ne!(cleared, stamped);
    let view = settings_view(&app, wid).unwrap();
    assert_eq!(view.feedback, None, "cleared about five seconds on");
    assert_eq!(view.messages_feedback_clock(), None);
    assert_eq!(
        app.messages_publish_due_at(),
        Some(cleared + TICK),
        "then only the minute tick"
    );
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
    // The page's own count of the log — the status line's words — as
    // the last publish left it.
    let status = |app: &App| {
        app.compiled_native_ui(wid)
            .expect("Settings fronts the window")
            .semantic(&crate::native_ui::UiKey::new("settings/messages/status"))
            .map(|node| node.label.clone())
            .expect("the Messages page is up")
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
    assert!(status(&app).starts_with("1 message"), "{}", status(&app));

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
    assert!(
        status(&app).starts_with("1 message"),
        "the page shows the last publish: {}",
        status(&app)
    );
    assert_eq!(app.messages_publish_due_at(), Some(opened + GAP));
    assert!(
        app.messages_deadline().is_some_and(|d| d <= opened + GAP),
        "the loop is asked back for the pending publish"
    );
    app.publish_native_messages_state_if_due(opened + GAP - std::time::Duration::from_millis(1));
    assert_eq!(
        app.messages_last_publish,
        Some(opened),
        "one millisecond short of the gap: still held"
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
    assert!(
        status(&app).starts_with("5 messages"),
        "…and the page shows all of it: {}",
        status(&app)
    );

    // Closing the last Settings tab drops the controller: the center
    // moving is nobody's business — no publish, nothing pending.
    assert!(app.close_settings_tabs());
    assert!(!settings_exists(&app), "no view, no controller");
    app.post_message(warn("after close"));
    assert_ne!(app.messages.revision(), app.messages_published_revision);
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
    assert!(status(&app).starts_with("6 messages"), "{}", status(&app));
}

/// REVIEW PIN (Phase 2, lens B, 2026-09-22): A 10 Hz RESTATE STORM
/// PUBLISHES AT 2 Hz AT MOST (design §4.2) — the path a downloading
/// update takes: `restate_message` every 100 ms for three seconds,
/// each followed by the park's own gate
/// (`publish_native_messages_state_if_due`), the park's clock stepped
/// in lock-step with the real instant each publish stamps, a view on
/// the route. Thirty restatements of the title: six publishes at most
/// (2 Hz × 3 s), at least five (a moving center is never starved), the
/// page two restatements behind inside a gap, and the last publish
/// carrying the last words.
#[test]
fn a_ten_hertz_restate_storm_publishes_at_two_hertz_at_most() {
    use crate::app_native::MESSAGES_PUBLISH_MIN_GAP as GAP;
    // Bound before the App, so the App (and its log writer) is dropped
    // first and the directory after it.
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-storm-")
        .tempdir()
        .expect("scratch dir");
    let mut app = App::headless_for_test();
    // A writer, as a real App has one: without it the page heads its list
    // with the not-saved note, and the row would start the second section.
    app.messages_log = crate::messages_store::Writer::spawn(&dir.path().join("messages.log"));
    let wid = WindowId(0);
    let meter = app.post_message(
        Message::new(tags::UPDATE, Severity::Info, "aterm update v0.48.0")
            .line("0%")
            .hold(Hold::Live {
                stale_after: aterm_messages::STALE_UPDATE,
            }),
    );
    // The row's title as the page shows it — on the first compact
    // section whatever the window's width.
    let title = |app: &App| {
        app.compiled_native_ui(wid)
            .expect("Settings fronts the window")
            .semantic(&crate::native_ui::UiKey::new(format!(
                "settings/messages/row/{}/title",
                meter.raw()
            )))
            .map(|node| node.label.clone())
            .expect("the entry is on the page")
    };
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    let opened = app
        .messages_last_publish
        .expect("opening Settings publishes");
    assert_eq!(title(&app), "aterm update v0.48.0");
    // The park's clock: virtual time from the opening publish, re-based
    // on the real instant each publish stamps, so the gate sees the
    // spacing the loop would.
    let step = std::time::Duration::from_millis(100);
    let mut base_real = opened;
    let mut base_virtual = std::time::Duration::ZERO;
    let mut publishes = 0usize;
    for k in 1..=30u32 {
        let before = app.messages_last_publish;
        let revision = app.messages.revision();
        assert!(app.restate_message(
            meter,
            Restatement {
                title: Some(format!("aterm update v0.48.0 \u{00b7} {k}%")),
                ..Restatement::default()
            },
        ));
        assert!(
            app.messages.revision() > revision,
            "a restatement moves the center"
        );
        let elapsed = step * k;
        let now = base_real + (elapsed - base_virtual);
        app.publish_native_messages_state_if_due(now);
        if app.messages_last_publish != before {
            publishes += 1;
            base_real = app.messages_last_publish.expect("published");
            base_virtual = elapsed;
            assert_eq!(
                app.messages_published_revision,
                app.messages.revision(),
                "tick {k}: a publish carries everything so far"
            );
        }
        if k == 7 {
            assert_eq!(
                title(&app),
                "aterm update v0.48.0 \u{00b7} 5%",
                "inside the gap the page keeps the last publish"
            );
            assert_eq!(app.messages_publish_due_at(), Some(base_real + GAP));
        }
    }
    assert!(
        (5..=6).contains(&publishes),
        "thirty restatements at 10 Hz: {publishes} publishes (2 Hz at most)"
    );
    assert_eq!(app.messages_published_revision, app.messages.revision());
    assert_eq!(
        title(&app),
        "aterm update v0.48.0 \u{00b7} 30%",
        "the last publish carries the last words"
    );
}

/// REVIEW PIN (Phase 2, lens B, 2026-09-22): THE MINUTE TICK IS A WAKE
/// THE LOOP MUST BE ASKED BACK FOR. Design §4.2: the projection
/// republishes once per 60 s while any Settings view is on the Messages
/// route so the relative times tick. `publish_native_messages_state_if_due`
/// honours the tick when the loop happens to run, but `messages_deadline`
/// folds only the 2 Hz hold-back (`messages_publish_due_at` is `None`
/// while the center is quiet), so an idle window on the page — no key,
/// no byte, no pointer — is never woken for it and `3 min ago` stays
/// `3 min ago`. With a standing row (the center arms nothing of its
/// own) and a view on the route, the deadline must be the tick.
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
    // The park at the tick publishes — this half holds.
    let mut probe = App::headless_for_test();
    probe.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    probe.post_message(
        Message::new(tags::UPDATE, Severity::Warn, "checks failing").hold(Hold::STANDING),
    );
    assert!(probe.open_settings_tab_in_window(wid, SettingsRoute::Messages));
    let probe_published = probe.messages_last_publish.unwrap();
    probe.publish_native_messages_state_if_due(probe_published + TICK);
    assert_ne!(
        probe.messages_last_publish,
        Some(probe_published),
        "the park ticks"
    );
    // …but nothing asks the loop to park there.
    assert_eq!(
        app.messages_deadline(),
        Some(published + TICK),
        "the tick must be armed as a wake while a view is on the route"
    );
    // A page nobody can see — the window occluded, minimized or headless
    // — arms no tick (audit 2026-09-24); revealed, it arms again.
    app.windows.get_mut(&wid).unwrap().occluded = true;
    assert_eq!(app.messages_deadline(), None, "a hidden page is no wake");
    app.windows.get_mut(&wid).unwrap().occluded = false;
    assert_eq!(app.messages_deadline(), Some(published + TICK));
}

/// The route words after *Open Settings* are the consent card's caption,
/// TERSE (ruling 46): the title before its dash, the route as
/// `detail[0]`, each further clause a line behind Details, the actions
/// withdrawn, the route's short hold.
#[test]
fn the_route_words_are_the_consent_cards_caption() {
    let route = "Privacy & Security \u{25b8} Full Disk Access";
    let opened = route_words(true, route);
    assert_eq!(
        opened.title.as_deref(),
        Some("Enable aterm in Full Disk Access"),
        "the instruction is the title, not the confirmation"
    );
    assert_eq!(opened.severity, None, "the question's tone");
    assert_eq!(
        opened.detail,
        Some(vec![
            route.to_string(),
            "find aterm there (+ adds a missing app)".to_string(),
            "if already enabled, leave it on".to_string(),
            "aterm keeps checking access".to_string(),
        ])
    );
    assert_eq!(opened.actions, Some(Vec::new()));
    assert_eq!(opened.hold, Some(Hold::For(HOLD_ROUTE)));
    assert!(
        HOLD_ROUTE <= std::time::Duration::from_secs(15),
        "a terse restate holds briefly: {HOLD_ROUTE:?}"
    );
    assert_eq!(opened.excerpt, Some(true), "the route is painted");
    let refused = route_words(false, route);
    assert_eq!(
        refused.title.as_deref(),
        Some("Open Privacy & Security \u{25b8} Full Disk Access"),
        "the route is the title, verb first"
    );
    assert_eq!(
        refused.detail,
        Some(vec![
            "Settings did not open".to_string(),
            "find aterm there (+ adds a missing app)".to_string(),
            "if already enabled, leave it on".to_string(),
        ]),
        "the outcome first behind it"
    );
    assert_eq!(refused.excerpt, Some(false), "the title alone on the glass");
    assert_eq!(
        refused.severity,
        Some(aterm_messages::Severity::Warn),
        "Settings not opening is a failure"
    );
    assert_eq!(
        refused.glyph,
        Some(aterm_messages::Severity::Warn.default_glyph())
    );
    assert!(
        refused.title.as_ref().unwrap().chars().count() <= 48,
        "fits at 60 columns beside the short Details form"
    );
    let full = route_words(
        false,
        "System Settings \u{25b8} Privacy & Security \u{25b8} Full Disk Access",
    );
    assert_eq!(
        full.title.as_deref(),
        Some("Open Privacy & Security \u{25b8} Full Disk Access"),
        "the route does not repeat System Settings"
    );
    assert_eq!(
        privacy_pane("full-disk-access"),
        Some(crate::menu::PrivacyPane::FullDiskAccess)
    );
    assert_eq!(privacy_pane("camera"), None);
}

/// `Open log` opens only an absolute, regular file under the log dir —
/// not a relative path, not a missing file, not a regular file OUTSIDE
/// the dir, not a `..` that lexically starts under the dir and escapes
/// it, not the dir itself, and not a SYMLINK planted in the dir (whatever
/// it points at, inside or out) — and a refusal is a Warn row naming the
/// path. The validator is pinned against a scratch dir standing in for
/// the log dir; the real one is asked only for what it must refuse.
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
    let refusal = log_did_not_open("/x/y.log");
    assert_eq!(refusal.title, "Couldn't open the log");
    assert_eq!(refusal.detail, vec!["/x/y.log"]);
    assert_eq!(refusal.tag, tags::SYSTEM);
    assert_eq!(
        refusal.hold,
        Hold::For(HOLD_GESTURE),
        "the person's own press"
    );
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
            .any(|l| l.msg.title == "Couldn't open the log"),
        "the refusal is a row"
    );
}

/// An instant past the grace of every row posted so far. Read AFTER the
/// post, never before it: `post` stamps the row with its own
/// `Instant::now()`, so a clock read ahead of the call falls inside the
/// grace whenever the thread is descheduled between the two for longer
/// than the margin (a loaded parallel run failed
/// `a_heavy_routine_row_folds_once_at_the_childs_exit_across_sub_passes`
/// that way, 2026-09-28).
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

/// THE `appstatus` GRAMMAR IS THE BARS', THE WORDS ARE THE ROW'S (ruling 116;
/// review 2026-09-24). The compat face promised byte-identical GRAMMAR:
/// field order, keys, percent-encoding and the phase/outcome words, the
/// bars' `activity_rows` format strings (status_bars.rs before 31b679459,
/// :982/:991) — while a `phase=live` line says what the row says NOW, so
/// its words follow the owner's rule. Pinned here in full, deliberately:
/// the first run's live line (`Installing ALab tools`, was `Installing
/// the ALab toolchain`; stats `3 of 10 programs`, the byte rollup behind
/// Details) and the download's; every line parses back into the bars'
/// seven (live) or eight (done) keys in order.
#[test]
fn appstatus_keeps_the_bars_grammar_in_the_rows_current_words() {
    let mut app = App::headless_for_test();
    // A person asked (Check for Updates): the download is their wait
    // and takes the row (design ruling 220).
    app.note_update_check_asked(true);
    app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    app.note_update_progress(&aterm_update::Progress::Downloading {
        version: "9.9.9".into(),
        bytes_done: 10_000_000,
        bytes_total: 74_000_000,
    });
    let rows = appstatus(&app);
    assert_eq!(
        rows,
        [
            "activity kind=toolchain phase=live progress=36/100 \
             title=Installing%20ALab%20tools detail=trust%20%C2%B7%20extracting \
             stats=3%20of%2010%20programs outcome=-",
            "activity kind=update phase=live progress=14/100 \
             title=Downloading%20aterm%20v9.9.9 detail=downloading \
             stats=10%20MB%20/%2074%20MB \
             outcome=-",
        ]
    );
    // A finished activity: the bars' `phase=done` grammar.
    app.record_message(toolchain_words::failed("install failed"));
    let rows = appstatus(&app);
    let done = rows.last().expect("the record's done line");
    assert!(done.starts_with("activity kind=toolchain phase=done progress=- title="));
    assert!(done.contains(" stats= outcome=warn since_ms="), "{done}");
    // Every line, parsed back: the bars' keys, in the bars' order, each
    // value one token (percent-encoded, so no space or newline forges a
    // field), the phase and outcome words the bars' own.
    for row in &rows {
        let mut words = row.split(' ');
        assert_eq!(words.next(), Some("activity"), "{row}");
        let fields: Vec<(&str, &str)> = words
            .map(|w| w.split_once('=').expect("key=value"))
            .collect();
        let keys: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
        let value = |key: &str| fields.iter().find(|(k, _)| *k == key).unwrap().1;
        match value("phase") {
            "live" => {
                assert_eq!(
                    keys,
                    [
                        "kind", "phase", "progress", "title", "detail", "stats", "outcome"
                    ]
                );
                assert_eq!(value("outcome"), "-");
            }
            "done" => {
                assert_eq!(
                    keys,
                    [
                        "kind", "phase", "progress", "title", "detail", "stats", "outcome",
                        "since_ms"
                    ]
                );
                assert_eq!(value("progress"), "-");
                assert!(["ok", "warn"].contains(&value("outcome")), "{row}");
            }
            other => panic!("phase={other}: {row}"),
        }
        assert!(["toolchain", "update"].contains(&value("kind")), "{row}");
        let progress = value("progress");
        assert!(
            progress == "-"
                || progress
                    .strip_suffix("/100")
                    .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| n <= 100)),
            "{row}"
        );
    }
}

/// SILENT BY DEFAULT: A ROUTINE PASS RAISES NO ROW (upstream dbf97ecff's
/// `a_routine_pass_raises_no_row_and_records_every_outcome`, ported onto
/// the center; the rulings on the origin/main merge, 2026-09-23). Every row
/// the toolchain lane used to raise for a pass that updates what is already
/// installed — its announcement, its live meter, "installed", the
/// managed-current row, the machine-settings rows, a deferred pass —
/// re-gridded every window and resized every running TUI. Pinned for the
/// whole marker set of a routine pass: no row, no re-grid, no deadline —
/// and every outcome on `appstatus` in the bars' ledger grammar, titled by
/// what happened (design ruling 67), in the order it arrived. The plan is
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
    app.post_machine_settings("spotlight-noindex 73 dir(s) migrated; universal-control disabled");
    app.record_message(toolchain_words::deferred(
        "an earlier toolchain pass is still running",
    ));
    app.toolchain_pass_ended(true);
    assert_eq!(app.messages.live_rows().count(), 0, "no row");
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(message_regrids(), regrids, "no re-grid for routine work");
    assert_eq!(app.messages_deadline(), None, "nothing to wake for");
    let enc = crate::control::pct_encode;
    let ledger = [
        ("ALab tools installed", "ty", "ok"),
        (
            "ALab tools checked",
            "the pass finished; 12 ALab program(s) are installed",
            "ok",
        ),
        (
            "Claude Code 2.1.280 and Codex 0.156.0 are current",
            "ready in every aterm tab",
            "ok",
        ),
        (
            "Spotlight now skips 73 build folders",
            "renamed with a .noindex suffix; cargo still builds there",
            "ok",
        ),
        (
            "ALab tools turned off Universal Control",
            "to turn it back on, set Universal Control on this Mac to leave and run: \
             defaults -currentHost delete com.apple.universalcontrol Disable; \
             defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges",
            "ok",
        ),
        (
            "ALab tools update postponed",
            "an earlier toolchain pass is still running",
            "ok",
        ),
    ];
    let rows = appstatus(&app);
    assert_eq!(rows.len(), ledger.len(), "{rows:#?}");
    for (row, (title, detail, outcome)) in rows.iter().zip(ledger) {
        let want = format!(
            "activity kind=toolchain phase=done progress=- title={} detail={} stats= \
             outcome={outcome} since_ms=",
            enc(title),
            enc(detail)
        );
        assert!(row.starts_with(&want), "{row}\n  want {want}");
    }
}

/// THE ONE ROW: THE FIRST-RUN ROW, THEN NO SUCCESS ROW (upstream
/// dbf97ecff/ac7942234's `a_first_run_shows_the_bar_then_folds_with_no_success_row`).
/// The pass that lays the default set onto a machine that had none of it
/// (multi-GB) keeps its live row — silence there reads as a hang — from the
/// announcement through the meter; the marker and the tailer's final read,
/// in either order, fold nothing while the child runs, and its EXIT folds
/// the row with no "installed" row after it; the roster is the record's.
/// Two re-grids: one open, one fold.
#[test]
fn a_first_run_shows_the_row_then_folds_with_no_success_row() {
    // Marker first, then the final read.
    let mut a = App::headless_for_test();
    let regrids = message_regrids();
    a.announce_toolchain_pass(
        "installing 10 ALab program(s) over the network (about 3 GB on disk when finished)",
        true,
    );
    assert_eq!(
        a.messages.live_rows().count(),
        1,
        "the announcement opens the row"
    );
    a.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    let live = a.messages.live_rows().next().expect("the live row");
    assert_eq!(live.msg.title, "Installing ALab tools");
    assert!(
        live.msg
            .meter
            .as_ref()
            .and_then(|m| m.fill_permille)
            .is_some_and(|f| f > 0 && f < 1000),
        "a live meter"
    );
    a.record_toolchain_outcome(toolchain_words::installed(
        "\u{2713} ALab toolchain installed: ay, trust \u{2014} ready in every aterm tab",
    ));
    a.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), true);
    assert_eq!(
        a.messages.live_rows().count(),
        1,
        "the child still runs: nothing folds yet"
    );
    a.toolchain_pass_ended(true);
    // Past the quiet AND the row's Complete echo, which a quiet shrink
    // waits out (design ruling 415 (b)): from a low fill it runs 1.65 s.
    a.settle_messages(Instant::now() + quiet_past_echoes());
    assert_eq!(a.messages.live_rows().count(), 0, "its exit folds the row");
    assert_eq!(a.message_band_rows, 0);
    assert_eq!(message_regrids(), regrids + 2, "one open, one fold");
    let rows = appstatus(&a);
    assert_eq!(
        rows.len(),
        1,
        "the roster is on record, and only it: {rows:?}"
    );
    let enc = crate::control::pct_encode;
    let want = format!(
        "activity kind=toolchain phase=done progress=- title={} detail={} stats= \
         outcome=ok since_ms=",
        enc("ALab tools installed"),
        enc("ay, trust \u{2014} ready in every aterm tab")
    );
    assert!(rows[0].starts_with(&want), "{}\n  want {want}", rows[0]);
    // The final read first, then the marker: the same glass.
    let mut b = App::headless_for_test();
    b.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    assert_eq!(b.messages.live_rows().count(), 1);
    b.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), true);
    b.apply_toolchain_snapshot(None, true);
    assert!(
        appstatus(&b)[0].contains("phase=live"),
        "the read claims no outcome"
    );
    b.record_toolchain_outcome(toolchain_words::ended("the toolchain pass finished", false));
    assert_eq!(b.messages.live_rows().count(), 1);
    b.toolchain_pass_ended(true);
    assert_eq!(b.messages.live_rows().count(), 0);
    assert_eq!(appstatus(&b).len(), 1, "the one record");
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

/// A FAILURE TAKES NO ROW EITHER (upstream dbf97ecff's
/// `a_failure_takes_no_row_and_folds_the_first_run_bar`). Now the
/// first-run row folds at its child's exit, the sentence is a Warn record,
/// and the failure's surface is the Settings ▸ Packages badge — so no meter
/// can be fused with a verdict at all, in either order.
#[test]
fn a_failure_takes_no_row_and_folds_the_first_run_row() {
    let mut failed_read = net_read(false, 1_700_000_000);
    if let Some(trust) = failed_read.file.programs.get_mut("trust") {
        trust.phase = atpkg::progress::Phase::Failed;
        trust.error = Some("no space left on device".into());
    }
    let mut a = App::headless_for_test();
    a.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    a.record_toolchain_outcome(toolchain_words::failed("partly installed: trust"));
    a.apply_toolchain_snapshot(Some(&failed_read), true);
    let live = a.messages.live_rows().next().expect("the child still runs");
    assert!(
        matches!(live.msg.hold, Hold::Live { .. }) && live.msg.severity == Severity::Info,
        "the live row, never a verdict on glass: {:?}",
        live.msg
    );
    a.toolchain_pass_ended(true);
    assert_eq!(a.messages.live_rows().count(), 0);
    let rows = appstatus(&a);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("outcome=warn") && rows[0].contains("partly%20installed"),
        "{}",
        rows[0]
    );
    let mut b = App::headless_for_test();
    b.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    b.apply_toolchain_snapshot(Some(&failed_read), true);
    b.record_toolchain_outcome(toolchain_words::failed("partly installed: trust"));
    b.toolchain_pass_ended(true);
    assert_eq!(
        b.messages.live_rows().count(),
        0,
        "the failed read raises nothing"
    );
}

/// `appstatus` against the bars' ledger on the withdrawals (review
/// 2026-09-22, re-cut for the silent lane and the latch of design §10.5
/// H8). A LATCHED row — a first run's — is folded by its child's exit
/// and by nothing else: neither its own vanished read nor the lane's
/// untagged clear at the waited child's exit. The fold WITHDRAWS it — the
/// log keeps the row, the face lists no finished activity and claims no
/// `ok` for a pass whose marker never came (ruling 13) — while the glass
/// ends its indicator by how the child exited. An announcement over a
/// live row supersedes it — one live row, nothing done.
#[test]
fn appstatus_lists_no_outcome_for_a_withdrawn_row() {
    let mut app = App::headless_for_test();
    app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
    let row = app
        .messages
        .live_by_key(toolchain_words::KEY_PASS)
        .map(|l| l.id)
        .expect("the first-run row");
    app.apply_toolchain_snapshot(None, true);
    assert!(
        app.messages.live(row).is_some(),
        "its exit folds it, not the read"
    );
    // The lane's own clear at a waited child's exit is untagged: a
    // latched row is left for the exit.
    app.apply_toolchain_snapshot(None, false);
    assert!(
        app.messages.live(row).is_some(),
        "latched: the exit's to fold"
    );
    app.toolchain_pass_ended(true);
    assert!(app.messages.live(row).is_none(), "withdrawn");
    assert_eq!(
        app.messages.log().get(row).unwrap().state,
        LogState::Retired(Retired::Withdrawn)
    );
    assert!(
        appstatus(&app).is_empty(),
        "no live row, and no finished activity claimed: {:?}",
        appstatus(&app)
    );
    // A LIVE row under the next first run's announcement is superseded,
    // not finished.
    let mut app = App::headless_for_test();
    app.announce_toolchain_pass("installing 2 ALab program(s)", true);
    app.announce_toolchain_pass("installing 3 ALab program(s)", true);
    let rows = appstatus(&app);
    assert_eq!(rows.len(), 1, "one live row, nothing done: {rows:?}");
    assert!(
        rows[0].starts_with(
            "activity kind=toolchain phase=live progress=- title=Installing%20ALab%20tools"
        ),
        "{}",
        rows[0]
    );
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

/// THE FIRST-RUN ANNOUNCEMENT OPENS THE ROW BEFORE ANY SNAPSHOT (upstream
/// dbf97ecff's `a_first_run_announcement_opens_the_toolchain_bar_before_any_snapshot`,
/// ported onto the App). The row goes up at the marker — before
/// `progress.json` exists — with atpkg's size, no meter, and live under
/// the announcement's cap rather than a fold. A pass that plans nothing
/// never opens a row of its own, and an announced one that ends empty
/// folds at its child's exit, claiming nothing; a ROUTINE pass's
/// announcement and reads — the every-6-hours no-op — stay invisible.
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
        assert_eq!(live.msg.tag, tags::TOOLCHAIN);
        assert_eq!(live.msg.title, "Installing ALab tools");
        assert!(
            live.msg.detail[0].contains("about 3 GB on disk when finished"),
            "{:?}",
            live.msg.detail
        );
        let meter = live
            .msg
            .meter
            .as_ref()
            .expect("busy before the file exists");
        assert!(
            meter.busy && meter.fill_permille.is_none(),
            "no fill before the file exists — the row moves instead: {meter:?}"
        );
        assert_eq!(meter.stats, "10 programs \u{00b7} ~3 GB", "how big");
        assert_eq!(
            live.msg.hold,
            Hold::Live {
                stale_after: aterm_messages::STALE_ANNOUNCE
            },
            "live: no fold…"
        );
        assert_eq!(live.fold_at, None);
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
    app.apply_toolchain_snapshot(Some(&empty_read(true)), true);
    assert_eq!(
        app.messages
            .live_by_key(toolchain_words::KEY_PASS)
            .map(|l| l.msg.title.as_str()),
        Some("Installing ALab tools"),
        "an announced row keeps its announcement until the plan lands"
    );
    app.apply_toolchain_snapshot(Some(&empty_read(false)), true);
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "the first run's child still runs"
    );
    app.toolchain_pass_ended(true);
    assert_eq!(
        app.messages.live_rows().count(),
        0,
        "announced, then ended empty: folded at exit"
    );
    assert!(
        appstatus(&app).is_empty(),
        "and it claims no outcome: {:?}",
        appstatus(&app)
    );
    // The every-6-hours no-op stays invisible: a routine pass's
    // announcement and reads raise nothing and move no grid.
    let mut routine = App::headless_for_test();
    let regrids = message_regrids();
    routine.announce_toolchain_pass("installing 1 program(s) over the network: ty", false);
    routine.apply_toolchain_snapshot(Some(&empty_read(true)), false);
    routine.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
    routine.apply_toolchain_snapshot(Some(&empty_read(false)), false);
    routine.toolchain_pass_ended(true);
    assert_eq!(routine.messages.live_rows().count(), 0);
    assert_eq!(message_regrids(), regrids, "no re-grid");
    assert!(appstatus(&routine).is_empty(), "{:?}", appstatus(&routine));
    assert_eq!(routine.messages_deadline(), None);
}

/// A PASS THAT IS OVER FOLDS ITS ROW AND CLAIMS NO OUTCOME (upstream
/// dbf97ecff/ac7942234's `a_pass_that_is_over_folds_its_bar_and_claims_no_outcome`,
/// its whole first-run × {clean, failed, dead writer} loop on the App). A
/// read of a pass that ended — cleanly, with a failure, or with its writer
/// gone — used to raise a held "all 10 installed" / "1 failed" /
/// "stopped" row; now it raises nothing, and the live row folds — at once
/// for a routine or a sibling's read, at the child's exit for a first
/// run's (whose ending read may be one sub-pass's). The outcome is the
/// markers' record and a failure's the Packages badge's.
#[test]
fn a_pass_that_is_over_folds_its_row_and_claims_no_outcome() {
    let over = |what: &str| {
        let mut read = net_read(false, 1_700_000_000);
        if what != "clean" {
            read.file.programs.get_mut("trust").unwrap().phase = atpkg::progress::Phase::Failed;
        }
        if what == "dead writer" {
            read.file.ended_unix = None;
        }
        read
    };
    for what in ["clean", "failed", "dead writer"] {
        for first_run in [true, false] {
            let mut app = App::headless_for_test();
            app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
            assert_eq!(app.messages.live_rows().count(), 1);
            // Whoever reads the pass as over, the row is LATCHED: its
            // child's exit folds it (design §10.2 #6).
            app.apply_toolchain_snapshot(Some(&over(what)), first_run);
            assert_eq!(
                app.messages.live_rows().count(),
                1,
                "{what}, first_run {first_run}: the child still runs"
            );
            app.toolchain_pass_ended(what == "clean");
            assert_eq!(
                app.messages.live_rows().count(),
                0,
                "{what}, first_run {first_run}: folded"
            );
            assert!(
                appstatus(&app).is_empty(),
                "{what}, first_run {first_run}: no outcome claimed: {:?}",
                appstatus(&app)
            );
            // Past the quiet and a clean pass's Complete echo (design
            // ruling 415 (b)).
            app.settle_messages(Instant::now() + quiet_past_echoes());
            assert_eq!(app.message_band_rows, 0, "{what}, first_run {first_run}");
            assert_eq!(
                app.messages_deadline(),
                None,
                "{what}, first_run {first_run}: nothing to wake for"
            );
        }
    }
}

/// A HELD ROW OUTLIVES THE FINAL SNAPSHOT, AND NONE NEVER ERASES IT
/// (upstream dbf97ecff's
/// `a_held_notice_outlives_the_final_snapshot_and_none_never_erases_it`,
/// re-cut: `appnotice` text is a record since design §10.5 H7, so the held
/// row here is a keyless toolchain row of the old notice's shape). No
/// tailed read — running or over, first run or not — replaces or re-words
/// it, and neither the vanished-file clear nor the child's exit folds it;
/// the first run's own row stands BESIDE it and folds at the exit.
#[test]
fn a_held_row_outlives_the_final_snapshot_and_none_never_erases_it() {
    let mut app = App::headless_for_test();
    let held = app.post_message(
        Message::new(
            tags::TOOLCHAIN,
            Severity::Warn,
            "aterm pkg install claude: 2.1.267 installed",
        )
        .hold(Hold::Default),
    );
    let mut over = net_read(false, 1_700_000_000);
    over.file.ended_unix = Some(1);
    app.apply_toolchain_snapshot(Some(&over), true);
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    {
        let live = app.messages.live(held).expect("never replaced by a read");
        assert_eq!(live.msg.meter, None);
        assert_eq!(live.revision, 0, "never re-worded");
        assert!(live.msg.hold.is_held());
    }
    assert_eq!(
        app.messages.live_rows().count(),
        2,
        "the first run's row stands beside it"
    );
    app.apply_toolchain_snapshot(None, true);
    app.apply_toolchain_snapshot(None, false);
    app.toolchain_pass_ended(true);
    assert!(
        app.messages.live(held).is_some(),
        "the lane's clear and the child's exit keep it"
    );
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "only the live row folded"
    );
}

/// `appnotice` TEXT IS RECORDED (design §10.5 H7; this replaces
/// `an_announcement_beside_a_held_notice_displaces_nothing`, whose held
/// notice row no longer exists): the text is on record and on
/// `appstatus` as a finished activity, never a row; a first run's
/// announcement beside it is the one row on the glass, and a routine one
/// raises nothing.
#[test]
fn appnotice_text_is_recorded() {
    let mut app = App::headless_for_test();
    let regrids = message_regrids();
    assert!(
        app.take_app_notice("toolchain", "aterm pkg install claude: 2.1.267 installed")
            .is_ok()
    );
    assert!(app.take_app_notice("update", "an operator's note").is_ok());
    assert_eq!(app.messages.live_rows().count(), 0, "no row");
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(message_regrids(), regrids, "no re-grid");
    let rows = appstatus(&app);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter().all(|r| r.contains(" phase=done ")),
        "records are finished activities: {rows:?}"
    );
    app.announce_toolchain_pass("installing 1", false);
    assert_eq!(
        app.messages.live_rows().count(),
        0,
        "a routine announcement raises nothing"
    );
    app.announce_toolchain_pass("installing 1 program(s) over the network: ty", true);
    assert_eq!(
        app.messages
            .live_by_key(toolchain_words::KEY_PASS)
            .map(|l| l.msg.title.as_str()),
        Some("Installing ALab tools"),
        "the new pass is on glass at once"
    );
    assert_eq!(app.message_band_rows, 1);
}

/// A FIRST RUN'S ROW WHOSE FEED WENT SILENT FOLDS AT ITS CAP (upstream
/// `a_live_bar_whose_feed_went_silent_folds_at_its_cap`, whose reads are
/// a first run's since dbf97ecff): no read for the tailed cap and the
/// meter is gone — and, as the bars' settle recorded every bar that left
/// the glass, the words it showed are on record. The center's own test
/// pins that a fresh read re-arms the cap (`center.rs`, "each restate
/// re-arms the cap").
#[test]
fn a_first_run_row_whose_feed_went_silent_folds_at_its_cap() {
    let mut app = App::headless_for_test();
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
    let now = Instant::now();
    let (stale_at, load_shown) = app
        .messages
        .live_by_key(toolchain_words::KEY_PASS)
        .map(|l| (l.stale_at, l.load_shown))
        .expect("the live row");
    let stale_at = stale_at.expect("the tailed cap");
    assert!(stale_at <= now + aterm_messages::STALE_TAILED);
    // The extraction's load words are up from the first frame (design
    // §10.6, review round 2): the cap is the one deadline.
    assert!(load_shown, "an extraction declares the disk, shown at post");
    assert_eq!(app.messages_deadline(), Some(stale_at));
    assert!(
        !app.settle_messages(now + aterm_messages::STALE_TAILED / 2),
        "not yet"
    );
    assert_eq!(app.messages.live_rows().count(), 1);
    assert!(
        app.settle_messages(now + aterm_messages::STALE_TAILED),
        "no report for the cap: gone"
    );
    assert_eq!(app.messages.live_rows().count(), 0);
    let rows = appstatus(&app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        // The log states the outcome (ruling 259): the live title is
        // the record's first detail line.
        rows[0].starts_with(
            "activity kind=toolchain phase=done progress=- title=Installing%20stopped \
             detail=Installing%20ALab%20tools "
        ),
        "the capped row leaves its words on record, as the bars' settle did: {}",
        rows[0]
    );
}

/// THE ROW OUTLIVES THE GLASS (upstream `a_folded_bar_leaves_its_sentence_in_the_ledger`
/// and `settle_folds_expired_bars_and_reports_the_row_change`, re-cut for
/// the owner's attention rule, design §10): a RECORD is on record at once
/// and never on the glass — an `appnotice` text since H7; a ROW's fold is
/// on record with the severity as the outcome — the ready row after its
/// hold (ok, with its honest sentence, never "restart"), then a stopped
/// lane's warning (warn). Each fold is reported as a glass change, the
/// record is never a row, and a full record is still an idle zero.
#[test]
fn a_folded_row_leaves_its_sentence_on_record() {
    let mut app = App::headless_for_test();
    assert!(
        app.take_app_notice("toolchain", "an operator's note")
            .is_ok()
    );
    app.post_message(update_words::staged(
        "0.48.0",
        99,
        Some(update_words::ApplyPosture::ManualByConfig),
    ));
    let now = Instant::now();
    let done = |app: &App| -> Vec<String> {
        appstatus(app)
            .into_iter()
            .filter(|r| r.contains(" phase=done "))
            .collect()
    };
    assert_eq!(app.message_band_rows, 1, "the decision alone is a row");
    let rows = done(&app);
    assert_eq!(rows.len(), 1, "the record is on record at once: {rows:?}");
    let enc = crate::control::pct_encode;
    assert!(
        rows[0].starts_with(&format!(
            "activity kind=toolchain phase=done progress=- title={} ",
            enc("an operator's note")
        )) && rows[0].contains(" outcome=ok "),
        "{}",
        rows[0]
    );
    assert!(!app.settle_messages(now), "nothing due yet");
    assert!(
        app.settle_messages(now + aterm_messages::HOLD_STAGED_MANUAL),
        "the ready row folds after its hold, and the glass changed"
    );
    let rows = done(&app);
    assert_eq!(rows.len(), 2);
    assert!(
        rows[1].starts_with("activity kind=update ") && rows[1].contains(" outcome=ok "),
        "oldest first — the order they left the glass: {rows:?}"
    );
    let staged = app
        .messages
        .log()
        .records()
        .rfind(|r| r.tag == tags::UPDATE)
        .map(|r| r.detail.join(" "))
        .expect("the staged row is on record");
    assert!(
        staged.contains("automatic install is off") && !staged.contains("restart"),
        "the record carries the honest sentence: {staged:?}"
    );
    // A warn row retires as a warn outcome, not an ok one.
    let later = now + aterm_messages::HOLD_STAGED_MANUAL;
    app.note_update_outcome(update_words::needs_install(
        "Couldn't install aterm v0.48.0",
        update_words::INSTALL_FROM_MENU,
        Severity::Warn,
        99,
    ));
    assert!(
        app.settle_messages(later + aterm_messages::HOLD_WARN),
        "then the stopped lane's warning"
    );
    let rows = done(&app);
    let last = rows.last().expect("recorded");
    assert!(
        last.starts_with("activity kind=update ") && last.contains(" outcome=warn "),
        "{last}"
    );
    app.settle_messages(
        later
            + aterm_messages::HOLD_WARN
            + aterm_messages::SHRINK_QUIET
            + std::time::Duration::from_millis(1),
    );
    assert_eq!(app.messages.live_rows().count(), 0);
    assert_eq!(
        app.message_band_rows, 0,
        "the record holds no rows on the glass"
    );
    assert_eq!(
        app.messages_deadline(),
        None,
        "FL-1: a full record is still an idle zero"
    );
}

/// The shape `appstatus` promises (upstream
/// `activity_rows_put_the_live_bars_above_the_finished_ones`, whose
/// failure goes straight to the ledger since dbf97ecff): live rows
/// first, then the finished ones, each carrying how it ended and how long
/// ago — a toolchain failure is a record from the start, never a row.
#[test]
fn appstatus_puts_the_live_rows_above_the_finished_ones() {
    let mut app = App::headless_for_test();
    // A person asked (Check for Updates): the download is their wait
    // and takes the row (design ruling 220).
    app.note_update_check_asked(true);
    app.record_toolchain_outcome(toolchain_words::failed("install failed"));
    assert_eq!(
        app.messages.live_rows().count(),
        0,
        "a failure goes straight to the record"
    );
    app.note_update_progress(&aterm_update::Progress::Downloading {
        version: "0.62.0".into(),
        bytes_done: 45,
        bytes_total: 90,
    });
    let rows = appstatus(&app);
    assert_eq!(rows.len(), 2, "one live, one finished: {rows:?}");
    assert!(
        rows[0].starts_with("activity kind=update phase=live progress=50/100 "),
        "the live download leads, with its meter: {}",
        rows[0]
    );
    assert!(
        rows[1].starts_with("activity kind=toolchain phase=done progress=- ")
            && rows[1].contains(" outcome=warn ")
            && rows[1].contains(" since_ms="),
        "the finished install follows, with how it ended: {}",
        rows[1]
    );
}

/// `seed-done:` is an ANSWER like the others (upstream `toolchain_ended`
/// → `retire_answered_toolchain`; the `Wake::PkgSeedDone` arm routes it
/// through [`App::record_toolchain_outcome`], as every other answer
/// arm does): its record goes on `appstatus`, and the live row folds —
/// unless a first-run child still runs, whose exit folds it.
#[test]
fn a_seed_done_answer_folds_the_live_row_unless_a_first_run_child_runs() {
    let answer = |first_run| {
        toolchain_words::ended(
            "the pass finished; 12 ALab program(s) are installed",
            first_run,
        )
    };
    let mut app = App::headless_for_test();
    app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
    // The `Wake::PkgSeedDone` arm's latch: a first run's answer is an
    // install (review 2026-09-24).
    assert!(app.toolchain_row_latched && app.toolchain_first_run);
    app.record_toolchain_outcome(answer(true));
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "the first run's child still runs"
    );
    app.toolchain_pass_ended(true);
    assert_eq!(app.messages.live_rows().count(), 0);
    let rows = appstatus(&app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains(" outcome=ok ") && rows[0].contains("title=ALab%20tools%20installed "),
        "{}",
        rows[0]
    );
    // A live row with no first-run child behind it (one carried across a
    // handoff) folds at the answer.
    let mut app = App::headless_for_test();
    app.post_message(toolchain_words::announced("installing 2 program(s)"));
    app.record_toolchain_outcome(answer(false));
    assert_eq!(app.messages.live_rows().count(), 0, "answered: folded");
    let rows = appstatus(&app);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].contains("title=ALab%20tools%20checked "),
        "a routine answer claims no update: {}",
        rows[0]
    );
}

#[cfg(unix)]
/// A parked outgoing handoff — the PARENT's half of the freeze predicate
/// — with nothing live behind it (the `app_update_handoff` tests' own
/// fixture, `parked_record`).
pub(crate) fn parked() -> crate::PendingUpdateHandoff {
    let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
    crate::PendingUpdateHandoff {
        park_at: Instant::now(),
        proof_ready_at: None,
        #[cfg(target_os = "macos")]
        activate_at_commit: false,
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
        update_health_said: carried.update_health_said,
        messages_captured_unix_ms: carried.captured_unix_ms,
    }
}

#[cfg(unix)]
/// THE PARENT'S RECORD IS ITS OWN TO THE END (design §3.7, revised on
/// review 2026-09-22), in PRODUCTION ORDER: the manifest captures the
/// carry, THEN the attempt parks (`app_update_handoff`, both sites). A
/// row posted under the park drains to the writer at once while its
/// hold waits; the flush the parent runs before it execs lands it on
/// disk; the carry ships rows, never lines. The SUCCESSOR's drain is the
/// one that waits — until Commit — so a refused successor wrote nothing.
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
    assert_eq!(
        app.carried_messages().next_message_id,
        during.raw() + 1,
        "the successor's ids continue past the parent's"
    );
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

    // THE SUCCESSOR before Commit: its lines wait; Commit drains them.
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
/// THE HANDOFF CARRY (design §3.8; the port of status_bars.rs:4775 and
/// app_restore.rs:4946-4961): the live rows cross the swap as plain words
/// in glass order and re-seed under the handoff's staleness cap with
/// their ids, words and holds; the update row moves on to "finishing"
/// only when this process is the handed-off successor AND the carry had
/// one, and a carried check warning stands (a landing answers only an
/// install warning, finding 54), never latched (round eight, finding
/// 301); no log
/// line crosses (the parent's record is its own to the end); an older
/// parent's `bars` map to rows and junk is dropped.
#[test]
fn a_handoff_carries_live_rows() {
    let mut parent = App::headless_for_test();
    let toolchain = parent.post_message(toolchain_words::announced("installing ay, trust"));
    parent.post_message(update_words::staged("0.76.0", 7, None));
    let installing = parent.post_message(update_words::installing("0.76.0"));
    let health = parent.post_message(update_words::health_warning(
        update_words::HealthKind::Check,
        "Update checks failing",
        "3 consecutive check failures",
    ));
    assert_eq!(parent.message_band_rows, 3);
    assert_eq!(
        parent.messages.log().pending_len(),
        0,
        "everything before the park was drained"
    );
    // The park: a record posted under it drains as any other (a
    // ledger-only row — two lines, nothing on the glass, no writer here).
    parent.pending_update_handoff = Some(parked());
    let logged = parent.post_message(
        Message::new(tags::SYSTEM, Severity::Info, "ledger only").hold(Hold::LogOnly),
    );
    assert_eq!(
        parent.messages.log().pending_len(),
        0,
        "the parent's drain never waits"
    );
    let carried = parent.carried_messages();
    let titles: Vec<&str> = carried.messages.iter().map(|m| m.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Update checks failing",
            "Installing ALab tools",
            "Installing aterm v0.76.0",
        ],
        "the glass rows, in glass order (the warning outranks the lane rows)"
    );
    assert!(carried.messages.iter().all(|m| m.on_glass));
    assert_eq!(carried.messages[2].id, installing.raw());
    assert_eq!(
        carried.messages[2].key.as_deref(),
        Some(update_words::KEY_PROGRESS)
    );
    assert_eq!(carried.messages[0].hold, "default");
    assert!(
        carried.messages[1].busy && carried.messages[2].busy,
        "the first-run row and the installing row moved; the carry says so"
    );
    assert_eq!(carried.next_message_id, logged.raw() + 1);
    let carry = window_carry(parent.message_band_rows, carried.clone());

    // THE SUCCESSOR, before Commit.
    let mut successor = App::headless_for_test();
    successor.incoming_handoff_pending = true;
    successor.message_band_rows = carry.status_bar_rows.min(MAX_ROWS);
    successor.seed_carried_messages(&carry, Some("0.76.0"));
    let row = successor.messages.live(toolchain).expect("the id is kept");
    assert_eq!(row.msg.title, "Installing ALab tools");
    assert!(
        row.msg.detail.is_empty(),
        "no size in the sentence: no line"
    );
    assert!(
        row.msg.meter.as_ref().is_some_and(|m| m.busy),
        "the busy flag rides the carry"
    );
    assert!(!row.is_queued(), "a glass row lands on the glass");
    assert_eq!(row.fold_at, None, "no hold before Commit…");
    assert!(row.stale_at.is_some(), "…only the handoff's cap");
    assert_eq!(row.msg.origin, aterm_messages::Origin::Carried);
    let update = successor
        .messages
        .live_by_key(update_words::KEY_PROGRESS)
        .expect("the progress row crossed");
    assert_eq!(update.id, installing);
    assert_eq!(update.msg.title, "Finishing aterm v0.76.0");
    assert_eq!(update.msg.detail, vec![update_words::FINISHING]);
    assert_eq!(
        successor.live_update_flow().map(|f| f.phase),
        Some(FlowPhase::Finishing),
        "the successor adopts the flow row as its own"
    );
    assert!(
        successor.messages.live(health).is_some(),
        "a landing proves nothing about checking: the check warning stands (finding 54)"
    );
    assert!(
        !successor
            .update_health_latched
            .contains(&update_words::HealthKind::Check),
        "…never latched: a latch only a proof lifts stood for the launch, and \
         muted this process's own next episode (round eight, finding 301)"
    );
    assert_eq!(
        successor.messages.log().next_id().raw(),
        carried.next_message_id + PARKED_PARENT_ID_RESERVE,
        "ids continue past the carry, above the band the parked parent records in"
    );
    assert_eq!(
        successor.messages.log().pending_len(),
        0,
        "nothing crossed as a log line"
    );
    assert!(
        !successor.sync_message_band_rows(Instant::now()),
        "the geometry is the parent's until Commit"
    );
    assert_eq!(successor.message_band_rows, 3);

    // Not the successor of a handoff: the words come back as they were.
    let mut plain = App::headless_for_test();
    plain.seed_carried_messages(&carry, None);
    assert_eq!(
        plain
            .messages
            .live_by_key(update_words::KEY_PROGRESS)
            .unwrap()
            .msg
            .title,
        "Installing aterm v0.76.0"
    );
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
    assert!(
        plain.messages.live(health).is_some(),
        "with no landing to prove it, the warning stands"
    );

    // No progress row carried: nothing is invented, and the health row
    // stands even for the successor.
    let mut healthy = App::headless_for_test();
    healthy.post_message(update_words::health_warning(
        update_words::HealthKind::Check,
        "Update checks failing",
        "x",
    ));
    let only_health = window_carry(1, healthy.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&only_health, Some("0.76.0"));
    assert!(
        successor
            .messages
            .live_by_key(update_words::KEY_PROGRESS)
            .is_none()
    );
    assert!(successor.messages.live_health().is_some());

    // An OLDER parent's `bars` (the status bars' carry, read until Phase 6):
    // the manifest still decodes — serde skips the key — and seeds nothing,
    // so the successor's own reporters say those lanes at Commit.
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
/// THE PARKED PARENT'S BAND IS RESERVED BEFORE THE SEED (design ruling
/// 415 (d)): the successor's boot ring holds ANOTHER writer's record under
/// the number of a row the parent carries. The seed moves that record to a
/// fresh id, and that id is above the band the parked parent keeps minting
/// in (`PARKED_PARENT_ID_RESERVE`) — never the parent's own next id, which
/// the parent hands to its next record before Commit. Reserving after the
/// seed (the order before round 43) put the squatter at `next_message_id`.
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
/// THE LANDING ANSWERS THE APPLY LANE'S LAST OUTCOME (round-seven update
/// audit, finding 30): "Update waits for you" (the automatic lane blocked,
/// then retried) and "Couldn't install aterm vX · Install now" (pressed,
/// and it landed) are carried on glass across the handoff that installed
/// the build; the successor's landing takes them down rather than
/// re-arming a fresh 45 s hold beside "Updated to aterm vX".
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

#[cfg(unix)]
/// A LANDING ANSWERS AN INSTALL WARNING ONLY (round-seven update audit,
/// finding 54): build X was staged while checks worked, then checks failed
/// and "Couldn't check for updates" went up; the person pressed Install
/// now and X landed. The updater's ledger still holds the check streak
/// (`record_apply_success` clears the apply streak only), so the successor
/// must not record "aterm updates work again" against the check warning,
/// nor take it down — and it does not raise it a second time: its class is
/// this process's updater's now, which restates a streak still failing
/// quietly (round eight, finding 301). A real check
/// (`HealthProof::Checked`) then heals it and records the healing. The
/// negative control: an install warning carried the same way IS answered
/// by the landing.
#[test]
fn a_landing_heals_only_an_install_warning() {
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    let mut parent = App::headless_for_test();
    assert!(parent.note_update_health_as(
        update_words::HealthKind::Check,
        "Couldn't check for updates",
        body,
    ));
    parent.post_message(update_words::installing("9.9.9"));
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.messages.after_handoff_commit(Instant::now());
    successor.post_update_landed("9.9.9", 7, 0);
    let healed = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.title == "aterm updates work again")
            .count()
    };
    assert_eq!(
        healed(&successor),
        0,
        "a landing proves nothing about checking: no healing on record"
    );
    assert_eq!(
        successor
            .live_update_health(|k| k == update_words::HealthKind::Check)
            .len(),
        1,
        "the check warning stands"
    );
    assert_eq!(
        UPDATER_ADOPTED_HEALTH.with(|adopted| std::mem::take(&mut *adopted.borrow_mut())),
        vec!["manifest"],
        "the class is this process's updater's: it is not announced a second time"
    );
    assert!(
        !successor.note_update_health(aterm_update::HEALTH_RESTATED_TITLE, body),
        "the successor's first failing check restates it, quietly"
    );
    successor.heal_update_health(HealthProof::Checked);
    assert_eq!(healed(&successor), 1, "a real check heals it, on record");
    assert!(
        successor
            .live_update_health(|k| k == update_words::HealthKind::Check)
            .is_empty()
    );

    // The negative control: an install warning is the landing's to answer.
    let mut parent = App::headless_for_test();
    assert!(parent.note_update_health(aterm_update::health_failing_title("apply"), body));
    parent.post_message(update_words::installing("9.9.9"));
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.messages.after_handoff_commit(Instant::now());
    successor.post_update_landed("9.9.9", 7, 0);
    assert_eq!(
        healed(&successor),
        1,
        "the landing heals an install warning"
    );
    assert!(successor.live_update_health(|_| true).is_empty());
}

#[cfg(unix)]
/// WHAT A HEALTH WARNING SAID crosses on the automatic lane too (round-seven
/// update audit, finding 31 — fixed by round six's finding 54, pinned
/// here): no progress row is carried, the warning is either still on glass
/// or already folded in the parent, and the landing still puts "aterm
/// updates work again" on the successor's record.
#[test]
fn an_automatic_landing_records_the_healing_of_a_carried_warning() {
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    for folded in [false, true] {
        let mut parent = App::headless_for_test();
        assert!(parent.note_update_health(aterm_update::health_failing_title("apply"), body));
        if folded {
            let past =
                Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(60);
            parent.settle_messages(past);
            assert!(
                parent.live_update_health(|_| true).is_empty(),
                "the warning folded in the parent"
            );
        }
        // No installing row: the automatic lane.
        let carry = window_carry(parent.message_band_rows, parent.carried_messages());
        let mut successor = App::headless_for_test();
        successor.seed_carried_messages(&carry, Some("0.76.0"));
        successor.post_update_landed("0.76.0", 7, 0);
        assert!(
            successor
                .messages
                .log()
                .records()
                .any(|r| r.title == "aterm updates work again"),
            "folded={folded}: the healing is on the successor's record"
        );
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
/// ROUND-EIGHT UPDATE AUDIT, FINDING 15 (the finding's own test) — A
/// CARRIED STALL THAT NEVER HEALED DOES NOT SILENCE THE SUCCESSOR'S OWN
/// WATCHDOG. The parent's checker stalled holding the check lane, so its
/// replacement could never check and the warning never healed; its row
/// folded, and a build staged before the stall landed on the automatic
/// lane. Round 44 latched every carried kind the landing does not answer,
/// and the successor had no proof that lifts a latched stall (an
/// up-to-date check is no progress, and its watchdog had announced
/// nothing): when its OWN fresh checker stalled, hours later, that was
/// "already said this launch" — a log line, no row — and nothing healed it
/// after. The carried stall is now the successor's watchdog's to heal, never
/// a latch: its own stall is said, with its row, and its first completed
/// check heals it. NEGATIVE CONTROL: a successor that carried nothing says
/// its stall the same way.
#[test]
fn a_carried_unhealed_stall_never_silences_the_successors_own_watchdog() {
    use crate::update_checker_watch::CheckerStall;
    use aterm_update::checker_watch::{CheckerBeat, CheckerPhase};
    let stalled = update_words::HealthKind::Stalled;
    for carried in [true, false] {
        let mut parent = App::headless_for_test();
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
/// the warning lifts, the healing is on record against the parent's words,
/// and the next episode speaks. Both lanes — the automatic one carries no
/// progress row, only what the parent said.
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
        // Still failing: the adopting updater restates it, quietly.
        assert!(!successor.note_update_health(aterm_update::HEALTH_RESTATED_TITLE, body));
        assert!(healings(&successor).is_empty());
        // Up to date: the adopting updater's recovery, naming the class.
        assert!(!successor.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, "manifest"));
        assert_eq!(
            healings(&successor),
            vec!["after \"Couldn't check for updates\", since Sep 14".to_string()],
            "the healing is on record against what the parent said (automatic: {automatic})"
        );
        assert!(successor.live_update_health(|k| k == check).is_empty());
        assert!(!owes_entry(&successor, check), "and its digest line");
        // A later episode is said: a row and its digest line.
        let later = "3 failed checks in a row since 2026-09-29T15:00:00Z: another reason.";
        assert!(
            successor.note_update_health(title, later),
            "a new failing episode is announced (automatic: {automatic})"
        );
        assert_eq!(successor.live_update_health(|k| k == check).len(), 1);
        assert!(owes_entry(&successor, check));
        // And its own healing is recorded against its own words.
        successor.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, "manifest");
        assert_eq!(
            healings(&successor).last().map(String::as_str),
            Some("after \"Couldn't check for updates\", since Sep 29")
        );
    }
    // NEGATIVE CONTROL: a successor that carried nothing hands nothing over.
    UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().clear());
    let _quiet = landed_successor(true, |_| {});
    assert!(UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow().is_empty()));
}

#[cfg(unix)]
/// A CARRIED WARNING'S "TO LOOK AT" LINE SAYS WHAT ITS PARENT'S WOULD HAVE:
/// "failing since Sep 14" and the updater's sentence, its cause. The
/// successor claimed it from the row's first line alone (`since Sep 14`,
/// all the carry held), so the line said when and never why. Both seamless
/// lanes, and the cold lane's carry through its wire. NEGATIVE CONTROL: a
/// parent older than the sentence's carry hands the excerpt alone, and the
/// line, still owed, says no more than that.
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
/// ROUND-EIGHT UPDATE AUDIT, FINDING 301 (scenario B), WITH 302's
/// REALISTIC NEXT EVENT — A CARRIED STALL WARNING IS THE SUCCESSOR'S
/// WATCHDOG'S TO HEAL. "Update check stopped" is about the parent's check
/// thread; the successor runs a new one, as a replacement generation does in
/// one process, and heals the warning by the same proof: its first completed
/// check, with the healing on record. NEGATIVE CONTROL: a beat that has
/// completed no check yet heals nothing.
#[test]
fn a_carried_stall_warning_heals_at_the_successors_first_completed_check() {
    let stalled = update_words::HealthKind::Stalled;
    let body = update_words::checker_stalled_body(3_000, "checking", true, false);
    for automatic in [false, true] {
        let mut successor = landed_successor(automatic, |parent| {
            assert!(parent.note_update_health_as(
                stalled,
                update_words::CHECKER_STALLED_TITLE,
                &body
            ));
        });
        assert!(!successor.update_health_latched.contains(&stalled));
        successor.look_at_update_checker(None, 100);
        successor.look_at_update_checker(Some(successor_beat(0, 0)), 110);
        assert!(
            healings(&successor).is_empty(),
            "NEGATIVE: no check has completed here yet (automatic: {automatic})"
        );
        assert!(owes_entry(&successor, stalled), "its digest line crosses");
        successor.look_at_update_checker(Some(successor_beat(1, 0)), 200);
        assert!(successor.live_update_health(|k| k == stalled).is_empty());
        assert!(
            !owes_entry(&successor, stalled),
            "its digest line heals with it"
        );
        assert_eq!(
            healings(&successor).len(),
            1,
            "this process's first completed check heals it (automatic: {automatic})"
        );
        assert!(
            healings(&successor)[0].starts_with("after \"Update check stopped\""),
            "{:?}",
            healings(&successor)
        );
        // A later stall of this process's own checker is said, with its row.
        successor.announce_update_checker_stall(
            crate::update_checker_watch::CheckerStall {
                generation: 1,
                for_secs: 3_000,
                phase: aterm_update::checker_watch::CheckerPhase::Checking,
                holds_lane: true,
                checks: 1,
            },
            None,
        );
        assert_eq!(successor.live_update_health(|k| k == stalled).len(), 1);
    }
}

#[cfg(unix)]
/// ROUND-EIGHT UPDATE AUDIT, FINDINGS 15 AND 301 (lock-held), WITH 302's
/// REALISTIC NEXT EVENT — A CARRIED LOCK-HELD WARNING IS THE SUCCESSOR'S
/// WATCHDOG'S TO HEAL. A landing proves nothing about a sibling aterm holding
/// the machine's check lock, but the successor's own checker sees the
/// streak's end: its first check completed without deferring (as for a
/// replacement generation, round six's finding 9). Latched with nothing to
/// heal it, the next lock-held episode was swallowed. NEGATIVE CONTROLS: no
/// checker yet, or one that has checked nothing, heals nothing; and a
/// sibling still stuck is taken over by the successor's own deferrals —
/// said once, not twice, healed when it ends.
#[test]
fn a_carried_lock_held_warning_is_the_successors_watchdog_to_heal() {
    use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
    let held = update_words::HealthKind::LockHeld;
    let rows = |app: &App| app.live_update_health(|k| k == held).len();
    for automatic in [false, true] {
        // The sibling let go before the successor's first check.
        let mut successor = landed_successor(automatic, |parent| {
            parent.announce_checker_lock_held(CHECKER_UNGATED_AFTER);
        });
        assert!(!successor.update_health_latched.contains(&held));
        successor.look_at_update_checker(None, 100);
        successor.look_at_update_checker(Some(successor_beat(0, 0)), 110);
        assert!(
            healings(&successor).is_empty(),
            "NEGATIVE: nothing here has seen the lock yet (automatic: {automatic})"
        );
        assert!(owes_entry(&successor, held), "its digest line crosses");
        successor.look_at_update_checker(Some(successor_beat(1, 0)), 200);
        assert_eq!(rows(&successor), 0);
        assert!(
            !owes_entry(&successor, held),
            "its digest line heals with it"
        );
        assert_eq!(
            healings(&successor).len(),
            1,
            "a check completed without deferring ends the streak (automatic: {automatic})"
        );
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
/// ROUND-SEVEN UPDATE AUDIT, GROUP RL (the review of the take-up) — ONLY A
/// WARNING THE LEDGER RAISED IS THE SUCCESSOR'S UPDATER'S TO HEAL. The window
/// reads a title it does not know as the check kind, and the strand notice
/// ("aterm is not updating on this machine", a channel this Mac cannot read)
/// is one: deliberately no ledger failure. Adopted as `manifest`, the
/// successor's first cycle found the ledger not failing — it never was — and
/// sent the check class's recovery: the strand's row came down and "aterm
/// updates work again" went on record over a machine still stranded. It is
/// now only remembered, with nothing handed to the updater; the successor
/// says its own strand once, as every process does, in one row.
/// CONTROLS: an older build's words for the check class are still a ledger
/// class, and adopted; a title this build does not know is not.
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
    // CONTROLS: the words decide, not the kind they are read as.
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
/// ROUND-EIGHT UPDATE AUDIT, FINDING 79 — A COLD-LANE LANDING RECORDS THE
/// HEALING OF THE PARENT'S INSTALL WARNING. The parent had said "Couldn't
/// install updates", then left with no terminal open: the cold lane `exec`s,
/// and its successor started with no memory of the warning — the window
/// carry is the seamless lanes' — so its landing recorded no healing and
/// Settings ▸ Messages kept the warning as the install lane's last word.
/// What the parent's warnings said now crosses the `exec`
/// (`bind_cold_lane_health_said`), and the successor takes it up before its
/// landing. NEGATIVE CONTROL: the same landing with nothing carried records
/// nothing; a value that does not parse is nothing.
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
    // The lanes are wired: the cold arm binds it, the window's entry takes it
    // and hands it to the App before its first present lands it.
    let handoff = include_str!("app_update_handoff.rs");
    assert!(
        handoff.contains("bind_cold_lane_health_said(&mut command, &self.carried_health_said());"),
        "the cold arm names it on its exec"
    );
    let main = include_str!("lib.rs");
    assert!(main.contains("app_update_handoff::take_cold_lane_health_said(just_updated)"));
    assert!(main.contains("app.take_up_carried_health_said(&cold_lane_health_said);"));
}

/// The Commit flush in the park tests, whose subject is what the successor
/// reads once the parent's lines have landed, never how fast they land:
/// a hang detector. Under the production 250 ms
/// (`MESSAGES_FLUSH_COMMIT_WAIT`) the writer was starved past it at load
/// 110-140 in 2 of 3 full `aterm-gui` runs (2026-09-30).
#[cfg(unix)]
const COMMIT_FLUSH_IN_TESTS: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

#[cfg(unix)]
/// THE LATE-PARK SUCCESSOR SEES WHAT ITS PARENT WROTE AFTER ITS BOOT
/// (round-seven update audit, finding 41): the successor loads the file at
/// its boot and its parent keeps recording until Commit. After Commit the
/// successor reads the file again — the parent flushed its last line
/// first — so a row the parent resolved after the boot is Resolved in its
/// history, never Stale, and a record written after the boot is there.
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
/// Round eight of the update audit, lead U62 (r8d #761): THE PARKED PARENT'S
/// ENDINGS HOLD IN THE SUCCESSOR. Rows cross only in the carry captured at the
/// park, and the parent's event loop runs on until Commit: a reporter's
/// resolve, a heal, a person's press there ended a row the carry still held
/// live. The successor kept every carried row live at Commit (the file's
/// ending notwithstanding), re-armed its full hold, and a healed warning came
/// back, healed a second time on the successor's own proof. A carried row
/// still in its carried words that the parent ended meanwhile ends here
/// WITHOUT a second log line, as a digest entry the parent filed already did,
/// and a health warning the parent healed is no longer this process's to heal.
///
/// NEGATIVE CONTROL: a carried row the parent left open stays live (the
/// engine's own test, `adversarial`'s
/// `a_carried_row_the_parent_ended_after_the_capture_ends_in_the_successor`,
/// keeps a fold on the parent's glass and a row restated here live too).
#[test]
fn a_row_the_parked_parent_ended_after_the_capture_stays_ended() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-messages-parked-endings-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(crate::messages_store::FILE_NAME);
    let mut parent = App::headless_for_test();
    parent.messages_log = crate::messages_store::Writer::spawn(&path);
    assert!(parent.note_update_health_as(
        update_words::HealthKind::Check,
        "Couldn't check for updates",
        "3 failed checks in a row since 2026-09-28T00:00:00Z: x",
    ));
    let resolved = parent.post_message(warn("a reporter ends this one").key("test.resolved"));
    let open = parent.post_message(warn("nobody ends this one").key("test.open"));
    parent.sync_messages();
    // The park: the carry is captured…
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    parent.pending_update_handoff = Some(parked());
    // …and the parent's loop runs on: a heal, a reporter's resolve.
    parent.heal_update_health(HealthProof::Checked);
    parent.resolve_message(resolved, Outcome::Ok);
    assert!(parent.flush_messages_log_within(COMMIT_FLUSH_IN_TESTS));
    drop(parent);

    let mut successor = App::headless_for_test();
    successor.messages_log = crate::messages_store::Writer::spawn(&path);
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.incoming_handoff_pending = false;
    successor.reload_messages_log_at_commit();
    successor.messages.after_handoff_commit(Instant::now());
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
    let healings = successor
        .messages
        .log()
        .records()
        .filter(|rec| rec.title == "aterm updates work again")
        .count();
    assert_eq!(healings, 1, "one healing, the parent's");
    drop(successor.messages_log.take());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
/// The review of round eight's lead U62 (r8d #761): ONLY A HEAL HAS PUT A
/// HEALING ON RECORD. The successor ends a carried health warning the parked
/// parent ended in any of four ways, and it used to forget what the warning
/// said and its latch for every one of them. A person's ✕ in the parent ends
/// the row and records no healing — the parent keeps what it said — so the
/// successor that forgot it recorded no healing when the failure later
/// healed. The row still ends; what it said and its day's entry stay, and
/// its healing is recorded once, by the proof here. CONTROL: a heal in the
/// parent puts the healing on record and leaves the successor nothing to
/// record, as the test above and the one below pin.
#[test]
fn a_health_warning_the_parked_parent_dismissed_is_still_healed_on_record() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-messages-parked-dismissal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(crate::messages_store::FILE_NAME);
    let mut parent = App::headless_for_test();
    parent.messages_log = crate::messages_store::Writer::spawn(&path);
    assert!(parent.note_update_health_as(
        update_words::HealthKind::Check,
        "Couldn't check for updates",
        "3 failed checks in a row since 2026-09-28T00:00:00Z: x",
    ));
    parent.sync_messages();
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    parent.pending_update_handoff = Some(parked());
    // The person presses ✕ on the warning while the parent is parked.
    let warning = parent.live_update_health(|_| true);
    assert_eq!(warning.len(), 1, "PRECONDITION: the warning is up");
    assert!(parent.messages.dismiss(warning[0], Instant::now()));
    assert!(parent.flush_messages_log_within(COMMIT_FLUSH_IN_TESTS));
    drop(parent);

    let mut successor = App::headless_for_test();
    successor.messages_log = crate::messages_store::Writer::spawn(&path);
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.incoming_handoff_pending = false;
    successor.reload_messages_log_at_commit();
    successor.messages.after_handoff_commit(Instant::now());
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
    let healings = successor
        .messages
        .log()
        .records()
        .filter(|rec| rec.title == "aterm updates work again")
        .count();
    assert_eq!(
        healings, 1,
        "its healing is recorded, once, by the proof here"
    );
    drop(successor.messages_log.take());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The parent of a seamless update, writing `messages.log` in a scratch
/// folder named for `name`: it does `raise`, the park captures its carry, it
/// does `while_parked` (its loop runs on until Commit) and flushes and
/// `_exit`s; its successor seeds that carry and commits, reading the file
/// again. Returns the committed successor, its writer still open, and the
/// folder.
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
/// The second review of round eight's lead U62 (r8d #761): A HEALING THE
/// PARKED PARENT RECORDED IS ON RECORD ONCE, WHATEVER ITS ROW DID. The
/// parent records a healing from what the warning said, not from its row:
/// a heal there with no live row still records it. The successor decided by
/// how the row ended, and two ways recorded it twice — a person's ✕ on the
/// warning before the heal (the first review's own regression: a dismissed
/// row kept the memory), and a warning that had folded off the glass before
/// the park, the usual state 45 s after it was said (no carried row at
/// all). The successor now reads the parent's record of the healing in the
/// file: what the warning said, its latch and its day's entry leave, and
/// its own proof records nothing more. A DOWNLOAD warning the parent's check
/// did not answer stays, healed on record here by its own proof.
///
/// And no row of the healed warning stands after Commit (the U5 group's
/// review): `a_kind_the_parked_parent_healed_leaves_no_row_whatever_the_file_says_of_it`
/// and `a_healed_warning_s_row_ends_though_another_writer_took_its_number`
/// are the arms where its ending in the file does not end it.
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
        // CONTROL: the kind the parent's check did not answer is still this
        // process's.
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
/// NEGATIVE CONTROL for the record the successor reads: only a healing
/// the parked parent wrote AFTER the capture (at or above the carry's next
/// id) answers a carried warning. A parent whose failure healed and then
/// came back in the very same words carries the second episode, and the
/// first episode's healing, below the carry's next id, is not its healing:
/// the second is recorded here, by the proof that answers it.
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
/// A KIND THE PARKED PARENT HEALED LEAVES NO ROW HERE, WHATEVER THE FILE
/// SAYS OF ITS ROW (round eight, the U5 group's review of lead U62). The
/// successor ends a carried row by the parent's ending of it in the file,
/// and only by the four endings that are verdicts on the row
/// (`end_carried_rows_the_parent_ended`); a healing the parent recorded is
/// read apart from that, as the healing it is. So a carried warning whose
/// row's ending the file does not give (here: none at all, the healer's
/// file holding only its own row and its healing) stood live with the
/// memory behind it gone, and came back on glass with a fresh hold at
/// Commit, healed — though the doc said the forgetting heals "as
/// `heal_update_health_where` does, less the record", which resolves every
/// live row of the kind. RED before the fix: the row stood.
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
/// A HEALED WARNING'S ROW ENDS THOUGH ANOTHER WRITER TOOK ITS NUMBER (round
/// eight, the U5 group's review of lead U62). A window sharing
/// `messages.log` posted under the very number the parent's warning row then
/// took; the parent healed that warning in the park, its row live, which
/// resolved the row. The file loads the other window's record under the
/// number and the parent's under a fresh id, so the successor, reading the
/// row's ending by the number alone, met the other record, kept the carried
/// row live, and put the healed "Couldn't check for updates" back on glass
/// at Commit. RED before the fix.
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
/// ANOTHER WINDOW'S HEALING IS NOT THE PARKED PARENT'S (round eight, the U5
/// group's review of lead U62). Every aterm of the user appends to one
/// `messages.log`, each minting ids from its own counter. Here a second
/// window, W2, said the same check warning (the same words: `since` is a
/// day) and healed it; the parent's own episode of it is still failing, is
/// carried, and is not healed in the park. The successor must still hold it
/// and still owe its day's entry. Before, any record in the healing's words
/// at an id at or above the carry's next id was taken for the parent's, and
/// three of W2's passed that:
///
/// * `w2-above-band-before`: W2's counter runs far above the parent's (it
///   launched later), and it healed before the capture;
/// * `w2-above-band-in-park`: the same, healed while the parent was parked
///   (after the capture): only the parent's id band tells it apart;
/// * `w2-in-band-before`: W2's healing took the very number the parent
///   would mint next, before the capture: only the capture's stamp tells it
///   apart.
///
/// RED before the fix on every arm: the warning, its latch and its day's
/// entry were dropped while it still failed (L5).
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
/// NO ID IS HANDED OUT TWICE ACROSS THE PARK (round-seven update audit,
/// finding 18, as reviewed): the carry's next id is taken at the park, and
/// the parked parent still records — an `appnotice`, a harness note — while
/// the successor records before Commit and at it ("Updated to aterm vX").
/// A reader that followed the parent to its last id K reads `since=K` from
/// the successor after Commit and sees every record the successor made;
/// the parent's parked records are in the successor's history, merged at
/// Commit before the landing; and messages.log names each id once.
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
/// THE FILE IS MERGED FIRST AT COMMIT (round-seven update audit, finding
/// 18, as reviewed): synchronously, before the Commit wake's first step
/// that can mint an id — the landing's record among them — so no record
/// of the parent's appears in this process's ring after, and below, an id
/// a `messages since=` reader was handed. The merge itself is the test
/// above; this pins where it runs.
#[test]
fn the_commit_wake_merges_the_parents_record_before_it_mints() {
    let src = include_str!("lib.rs");
    let wake = src
        .find("Wake::ActivateCommittedHandoff { mut expected } => {")
        .expect("the Commit wake");
    let body = &src[wake..];
    let reload = body
        .find("self.reload_messages_log_at_commit();")
        .expect("the Commit wake merges the file");
    for mints in [
        "self.hand_carried_restored_agents();",
        "self.window_event(el, winit_id, deferred);",
        // The landing's record is minted inside the freeze's lift, which
        // commits the presence fold and the band's growth as one re-grid
        // (`App::release_successor_commit_freeze`, pinned below).
        "self.release_successor_commit_freeze();",
    ] {
        let at = body.find(mints).expect(mints);
        assert!(reload < at, "the merge runs before `{mints}`");
    }
    let presence = include_str!("app_presence.rs");
    let lift = presence
        .find("pub(crate) fn release_successor_commit_freeze(&mut self) {")
        .expect("the freeze's lift");
    let lift_end = presence[lift..]
        .find("\n    }\n")
        .map(|e| lift + e)
        .expect("the lift's end");
    assert!(
        presence[lift..lift_end].contains("app.post_update_landed("),
        "the lift mints the landing's record"
    );
    assert!(
        !src.contains("MessagesLogReloaded"),
        "no later, asynchronous merge"
    );
}

#[cfg(unix)]
/// THE PARENT'S RECORD LANDS BEFORE COMMIT `_exit`S IT (round-seven update
/// audit, finding 53): the lanes that never exec flush, bounded, as the
/// ProofReady arm's last step before `commit_and_exit` — after every step
/// that may still record, so nothing the writer thread holds dies with
/// the process. The flush's own behaviour is the test above; this pins
/// where it runs: last in what the claim answers, after the video answer
/// (`App::answer_at_commit_claim`), and again, one fence grain at most,
/// in a held claim's pass for what its fence recorded (round eight, #18).
#[test]
fn the_seamless_parent_flushes_its_record_right_before_commit() {
    let src = include_str!("app_update_handoff.rs");
    let answers = src
        .find("    fn answer_at_commit_claim(")
        .expect("what the claim answers");
    let answers_end = answers + src[answers..].find("\n    }\n").expect("its end");
    let body = &src[answers..answers_end];
    let video = body
        .find("let video = self.video_answer_before_commit(VIDEO_EXPORT_COMMIT_WAIT);")
        .expect("the claim answers video");
    let flush = body
        .find("self.flush_messages_log_within(MESSAGES_FLUSH_COMMIT_WAIT);")
        .expect("and flushes the message log");
    assert!(video < flush, "after the video answer");
    let records = |text: &str| text.contains("record_message(") || text.contains("post_message(");
    assert!(!records(&body[flush..]), "nothing records after it there");
    let answered = src
        .find("let answered_in = self.answer_at_commit_claim(child_pid);")
        .expect("the ProofReady arm runs it");
    let held_flush = answered
        + src[answered..]
            .find("self.flush_messages_log_within(HANDOFF_INPUT_DRAIN_YIELD);")
            .expect("a held claim flushes what its fence recorded");
    let commit = answered
        + src[answered..]
            .find("crate::seamless::commit_and_exit(commit_fd, proof)")
            .expect("and commits");
    assert!(
        held_flush < commit,
        "the flush runs before the Commit write"
    );
    assert!(
        !records(&src[answered..commit]),
        "nothing records between the flushes and the Commit write"
    );
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
/// A ROW ABOUT THE OLD PROCESS LEAVES WITH IT (round-seven update audit,
/// finding 42): "Screen reader access lost · restart aterm to retry" is
/// answered by the successor, a new process with its own publisher, so a
/// carried one never stands (Standing) over access that works.
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
/// A CARRY WITH NO ROWS STILL CARRIES THE PARENT'S NEXT ID (round-seven
/// update audit, finding 18): the automatic lane parks with nothing on the
/// band, while the parent recorded past what the successor loaded at its
/// boot — so the successor's first record after Commit must still be
/// numbered above every id the parent handed out, or a `messages since=`
/// reader skips it.
#[test]
fn a_carry_with_no_rows_still_raises_the_successors_ids() {
    let mut parent = App::headless_for_test();
    for n in 0..10 {
        parent.record_message(warn(&format!("record {n}")).hold(Hold::LogOnly));
    }
    let carried = parent.carried_messages();
    assert!(carried.messages.is_empty(), "nothing is on the band");
    assert_eq!(carried.next_message_id, 11);
    let carry = window_carry(0, carried);
    let mut successor = App::headless_for_test();
    successor.incoming_handoff_pending = true;
    successor.seed_carried_messages(&carry, Some("0.99.0"));
    successor.incoming_handoff_pending = false;
    let id = successor.record_message(warn("after commit").hold(Hold::LogOnly));
    assert!(
        id.raw() >= 11,
        "the successor's first id is above the parent's: {id}"
    );
    let rows = successor.read_messages(&wire::ReadQuery {
        since: MessageId::from_raw(10),
        ..wire::ReadQuery::default()
    });
    assert!(
        rows.iter().any(|row| row.contains("after%20commit")),
        "a reader past the parent's last id sees it: {rows:?}"
    );
}

#[cfg(unix)]
/// AFTER COMMIT the carried rows take their own lifetimes back (design
/// §3.8): a held row on glass anchors its hold at the commit instant and
/// folds on it — into the ledger, as Folded — a standing row stands, and
/// the band shrinks after the D1 quiet. Before Commit nothing folds on a
/// hold and the row count never moves. And the one state in which the
/// two counts disagree — a row the parent's frozen count had queued —
/// converges on the center's count at the first unfrozen sync.
#[test]
fn carried_messages_reseed_live_and_fold_after_commit() {
    let mut parent = App::headless_for_test();
    let tools = vec!["ay".to_string()];
    let pill = toolchain_words::seed_pill_text(&tools, None, 0, HookDialect::Zsh);
    // A held Success row (the record's words at the default hold, as a
    // build before the silent lane carried them).
    let installed = parent.post_message(held_row(&toolchain_words::installed(&pill)));
    // A STANDING row (the health warning holds its 45 s since the merge's
    // ruling 59; a standing row is the GPU-lost and a11y lanes' shape).
    let health = parent.post_message(
        update_words::health_warning(
            update_words::HealthKind::Check,
            "Update checks failing",
            "3 consecutive check failures",
        )
        .hold(Hold::STANDING),
    );
    assert_eq!(parent.message_band_rows, 2);
    parent.pending_update_handoff = Some(parked());
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    assert_eq!(carry.status_bar_rows, 2);

    let mut successor = App::headless_for_test();
    successor.incoming_handoff_pending = true;
    successor.message_band_rows = carry.status_bar_rows.min(MAX_ROWS);
    successor.seed_carried_messages(&carry, None);
    assert_eq!(successor.messages.live_rows().count(), 2);
    assert_eq!(successor.messages.committed_rows(), 2);
    // A hold that would be due: nothing folds before Commit — the rows
    // have no hold yet, and the freeze would decline one anyway.
    let now = Instant::now();
    let past_success = now + HOLD_SUCCESS + std::time::Duration::from_secs(1);
    assert!(!successor.settle_messages(past_success));
    assert!(successor.messages.live(installed).is_some());
    assert_eq!(successor.message_band_rows, 2, "frozen");

    // COMMIT — what the `ActivateCommittedHandoff` arm does.
    successor.incoming_handoff_pending = false;
    successor.messages.after_handoff_commit(now);
    successor.sync_messages();
    let row = successor.messages.live(installed).unwrap();
    assert_eq!(
        row.fold_at,
        Some(now + HOLD_SUCCESS),
        "the success's own hold, from Commit"
    );
    assert_eq!(row.stale_at, None, "the handoff cap is gone");
    let standing = successor.messages.live(health).unwrap();
    assert_eq!(standing.fold_at, None);
    assert_eq!(standing.stale_at, None, "a standing row stands");
    assert_eq!(successor.message_band_rows, 2);
    // The hold falls due: the row folds into the ledger; the band keeps
    // its height through the D1 quiet and gives the row back after it.
    assert!(successor.settle_messages(past_success));
    assert!(successor.messages.live(installed).is_none());
    assert_eq!(
        successor.messages.log().get(installed).unwrap().state,
        aterm_messages::LogState::Retired(aterm_messages::Retired::Folded)
    );
    assert!(successor.messages.live(health).is_some());
    assert_eq!(successor.message_band_rows, 2, "shrink after the quiet…");
    assert!(successor.settle_messages(past_success + aterm_messages::SHRINK_QUIET));
    assert_eq!(successor.message_band_rows, 1, "…not before");
    assert_eq!(
        successor.messages.log().pending_len(),
        0,
        "the successor's own lines drained on the first sync after Commit (no writer here: dropped at the seam)"
    );

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

/// NO TOOLCHAIN ANSWER ARM POSTS A ROW (origin/main merge review,
/// 2026-09-23). The merge's first cut left `Wake::PkgSeedDone` on
/// `post_message(toolchain_words::ended(..))`: the record still landed,
/// but the live row never folded at the answer and every window repainted
/// for a record (ruling 34). The arms need an event loop, so the seam is
/// pinned in the source: in `lib.rs` a `toolchain_words` builder reaches
/// `post_message` only for an `appnotice` text (R34); every pass answer
/// goes through [`App::record_toolchain_outcome`] or
/// [`App::record_message`]. Whitespace is dropped first, so a rustfmt
/// line break between the call and its builder cannot hide one.
#[test]
fn no_toolchain_answer_arm_posts_a_row() {
    let source: String = include_str!("lib.rs").split_whitespace().collect();
    let mut offenders = Vec::new();
    for prefix in [
        concat!("post_message(", "toolchain_words::"),
        concat!("post_message(", "crate::toolchain_words::"),
    ] {
        for (at, _) in source.match_indices(prefix) {
            let builder = source[at + prefix.len()..]
                .split('(')
                .next()
                .unwrap_or_default();
            if builder != "appnotice" {
                offenders.push(builder.to_string());
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a toolchain answer raises a row instead of recording through \
         `record_toolchain_outcome` / `record_message`: {offenders:?}"
    );
}

// ---- Attention and motion (design §10.5, §10.8, §10.12) --------------

/// A LIGHT routine read: the same pass as [`net_read`] with a plan far
/// under [`toolchain_words::HEAVY_PASS_BYTES`] — the pass that paints
/// nothing.
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

/// An automatic update is SILENT (design ruling 220, the owner's R3): its
/// download takes no row and no re-grid — Settings ▸ Software Update says
/// how far it has got — and its staged build is a RECORD (design §10.5
/// H2), the words in the log.
#[test]
fn an_automatic_staged_build_is_a_record_and_leaves_no_row() {
    let mut app = App::headless_for_test();
    let regrids = message_regrids();
    app.note_update_progress(&aterm_update::Progress::Downloading {
        version: "9.9.9".into(),
        bytes_done: 10_000_000,
        bytes_total: 74_000_000,
    });
    assert_eq!(app.messages.live_rows().count(), 0, "no row: nobody waits");
    let page = app.update_snapshot(false).projection();
    assert_eq!(page.headline, "Downloading aterm v9.9.9\u{2026}");
    assert_eq!(page.detail.as_deref(), Some("10 MB / 74 MB downloaded."));
    app.note_update_progress(&aterm_update::Progress::Verifying {
        version: "9.9.9".into(),
    });
    assert_eq!(app.messages.live_rows().count(), 0, "nor for its check");
    let page = app.update_snapshot(false).projection();
    assert_eq!(
        page.headline, "Downloading aterm v9.9.9\u{2026}",
        "the download's headline; the detail alone says it is checked"
    );
    assert_eq!(page.detail.as_deref(), Some("Checking the download."));
    // The App's own posture — a headless App's is the handoff-off
    // record; the automatic lane's is a record too.
    app.note_update_progress(&aterm_update::Progress::Staged {
        version: "9.9.9".into(),
        build: 41,
    });
    app.settle_messages(Instant::now() + PROGRESS_GRACE * 2);
    assert_eq!(app.messages.live_rows().count(), 0, "no row");
    assert!(app.messages.echoes().is_empty(), "and no echo");
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(
        message_regrids(),
        regrids,
        "an automatic update costs no re-grid"
    );
    assert!(app.update_quiet.is_none(), "the page reads the stage now");
    let rec = recorded(&app, update_words::KEY_PROGRESS).expect("the staged record");
    assert_eq!(rec.title, "aterm v9.9.9 is ready");
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
        assert_eq!(rec.title, "aterm v9.9.9 is ready", "asked={asked}");
    }
}

/// A download nobody is waiting on that FAILS is a record in the flow's
/// words, the version its report did not carry kept from the quiet
/// download (design ruling 220): no row came, so no echo goes.
#[test]
fn an_automatic_download_that_fails_is_a_record_with_its_version() {
    let mut app = App::headless_for_test();
    app.note_update_progress(&aterm_update::Progress::Downloading {
        version: "9.9.9".into(),
        bytes_done: 10_000_000,
        bytes_total: 0,
    });
    assert_eq!(
        app.update_snapshot(false).projection().detail.as_deref(),
        Some("10 MB downloaded.")
    );
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
            .any(|r| r.title == "Couldn't download aterm v9.9.9"),
        "the failure's record names the version"
    );
}

/// A RATE-LIMITED DOWNLOAD DID NOT FAIL (review 2026-09-24): a GitHub
/// rate limit (`Progress::Deferred`) heals by itself and retries on the
/// next check, so the revealed download row leaves with no outcome to
/// claim — a fade, never `⚠ failed` — and the deferral is a record. Nor
/// did a release WITHHELD while it downloaded (round eight #72: its signer
/// revoked, a yank, a quarantine). A real failure still ends in its Fault
/// echo.
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
        // A person's check: its download has the row (design ruling 220).
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

/// THE LANDING (design §10.5 H4): the live Finishing row is resolved
/// `Ok` — its Complete echo plays — and the good news is a record.
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

/// A NAVIGATION press that took the person to fix the thing READ the row
/// (design §10.5 H6): a held row folds; a live row stays.
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

/// A REFUSED INSTALL (design §10.2 #7): the Installing row an explicit
/// apply added resolves `Warn` — a refusal is not a delivery — and its
/// indicator ends in the Fault echo.
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
    let live = app
        .messages
        .live_rows()
        .next()
        .expect("the heavy pass's row");
    assert_eq!(live.msg.title, "Updating ALab tools");
    assert_eq!(app.message_band_rows, 0, "inside its grace, off the glass");
    assert_eq!(message_regrids(), regrids);
    app.settle_messages(past_the_grace());
    assert_eq!(app.message_band_rows, 1, "revealed once the work lasted");
    assert_eq!(message_regrids(), regrids + 1);
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "a read restates in place"
    );
    assert_eq!(message_regrids(), regrids + 1);
}

/// A PERSON'S CHECK TAKES ITS ROW (design ruling 224): they pressed Check
/// & Update Now and wait for it, so its pass is theirs whatever it weighs
/// — the 250 MB bar is for work nobody asked for. A no-op Check (1–4 s,
/// `packages.log`) ends inside the grace and never touches the glass; a
/// longer one shows `Checking ALab tools`, busy, becomes the plan's fill
/// (`Updating ALab tools`, a light 40 MB pass included), holds across the
/// markers' records and the pass's ending read, and folds at the child's
/// exit with its Complete echo in its own finished words. Two re-grids.
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
    assert_eq!(live.msg.title, "Checking ALab tools");
    assert!(live.msg.meter.as_ref().is_some_and(|m| m.busy), "busy");
    assert_eq!(attention(&live.msg), Ok(Attention::Progress));
    let mut t = past_the_grace();
    app.settle_messages(t);
    assert_eq!(app.message_band_rows, 1, "the wait lasted: revealed");
    // The plan lands: a LIGHT pass paints, because a person waits.
    app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_000)), false);
    let live = app.messages.live_rows().next().expect("restated");
    assert_eq!(app.messages.live_rows().count(), 1, "in place");
    assert_eq!(live.msg.title, "Updating ALab tools");
    let meter = live.msg.meter.as_ref().expect("a meter");
    assert!(meter.fill_permille.is_some_and(|f| f > 0), "a fill");
    assert!(meter.amount.is_some(), "the ETA's input");
    assert_eq!(meter.stats, "3 of 10 programs");
    assert_eq!(live.msg.finished_title(), "ALab tools updated");
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
    assert_eq!(echo.msg.finished_title(), "ALab tools updated");
    for _ in 0..40 {
        t += std::time::Duration::from_millis(100);
        app.settle_messages(t);
    }
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(message_regrids(), regrids + 2, "one open, one fold");
    // The outcome is the markers' record, as before.
    assert!(
        appstatus(&app)
            .iter()
            .any(|r| r.contains("title=ALab%20tools%20checked")),
        "{:?}",
        appstatus(&app)
    );
    // Nothing is left behind: the next light routine pass is silent again.
    app.apply_toolchain_snapshot(Some(&light_read(true, 1_700_000_500)), false);
    assert_eq!(app.messages.live_rows().count(), 0, "nobody waits on it");
}

/// A CHECK QUEUED AT THE STORE LOCK SAYS SO (ruling 224): the wait that
/// used to be a log line — up to the 30-minute bound behind the six-hourly
/// pass or the head watch — is `Waiting for another ALab update`, busy
/// with its elapsed clock, back to `Checking ALab tools` once the lock is
/// taken. The LANE's child exiting meanwhile (the holder, unlatched)
/// leaves the person's row alone; a wait that runs out with nothing
/// claimed vanishes.
#[test]
fn a_queued_check_says_what_it_waits_for() {
    use crate::message_reporters::{PackagesVerb, WAITING_FOR_ALAB};
    let mut app = App::headless_for_test();
    app.person_pass(PersonPass::Began(PackagesVerb::Check));
    app.person_pass(PersonPass::Waiting);
    let title = |app: &App| {
        app.messages
            .live_by_key(toolchain_words::KEY_PASS)
            .map(|l| l.msg.title.clone())
    };
    assert_eq!(title(&app).as_deref(), Some(WAITING_FOR_ALAB));
    assert_eq!(app.messages.live_rows().count(), 1, "one row, restated");
    app.toolchain_pass_ended(true);
    assert_eq!(
        title(&app).as_deref(),
        Some(WAITING_FOR_ALAB),
        "the holder's exit is not the person's"
    );
    app.person_pass(PersonPass::Running);
    assert_eq!(title(&app).as_deref(), Some("Checking ALab tools"));
    app.person_pass(PersonPass::Ended(EchoKind::Vanish));
    assert_eq!(app.messages.live_rows().count(), 0);
    // With no verb in flight the wait words are not the lane's to say.
    app.person_pass(PersonPass::Waiting);
    assert_eq!(app.messages.live_rows().count(), 0);
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
    let mut planned = App::headless_for_test();
    assert_eq!(run(&mut planned, true), "ALab tools updated");
}

/// A PERSON'S INSTALL (ruling 225): the minutes-long ~3 GB wait Settings
/// starts shows the size once atpkg announces it (`10 programs · ~3 GB`),
/// then the plan's fill under the install's own title, and a failed one
/// ends in a Fault echo (the page says why; the Packages badge and the
/// record keep it). A Remove is busy with the disk as its load.
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
    assert_eq!(live.msg.title, "Installing ALab tools");
    let meter = live.msg.meter.as_ref().expect("busy meter");
    assert!(meter.busy);
    assert_eq!(meter.stats, "10 programs \u{b7} ~3 GB");
    assert_eq!(meter.load, Some(aterm_messages::Load::Network));
    assert_eq!(attention(&live.msg), Ok(Attention::Progress));
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
    let live = app.messages.live_rows().next().expect("restated");
    assert_eq!(live.msg.title, "Installing ALab tools", "not `Updating`");
    assert_eq!(live.msg.finished_title(), "ALab tools installed");
    app.person_pass(PersonPass::Ended(EchoKind::Fault));
    assert_eq!(app.messages.live_rows().count(), 0);

    let mut rm = App::headless_for_test();
    rm.person_pass(PersonPass::Began(PackagesVerb::Remove));
    let live = rm.messages.live_rows().next().expect("the Remove's row");
    assert_eq!(live.msg.title, "Removing ALab tools");
    assert_eq!(
        live.msg.meter.as_ref().and_then(|m| m.load),
        Some(aterm_messages::Load::Disk)
    );
    assert_eq!(live.msg.finished_title(), "ALab tools removed");
    assert_eq!(attention(&live.msg), Ok(Attention::Progress));
    rm.person_pass(PersonPass::Ended(EchoKind::Complete));
    assert_eq!(rm.messages.live_rows().count(), 0);
}

/// A person's Check behind a FIRST RUN: the lane's latched row keeps
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
    // The person's own tailer follows the same file (`first_run` false):
    // the lane's tailer paints the first run, in the first run's words
    // (ruling 229) — no turn-about with `Updating`, and its echo is not
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

/// ONE OPEN, ONE FOLD across the child's sub-passes (design §10.2 #6): a
/// heavy routine row is LATCHED — a sub-pass's answer and its ending read
/// fold nothing — and the child's exit folds it with its Complete echo.
#[test]
fn a_heavy_routine_row_folds_once_at_the_childs_exit_across_sub_passes() {
    let mut app = App::headless_for_test();
    let regrids = message_regrids();
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), false);
    let mut t = past_the_grace();
    app.settle_messages(t);
    assert_eq!(app.message_band_rows, 1);
    // The vendor sub-pass answers and ends; the ALab set's reads follow.
    app.record_toolchain_outcome(toolchain_words::ended("the vendor pass finished", false));
    app.apply_toolchain_snapshot(Some(&net_read(false, 1_700_000_000)), false);
    assert_eq!(
        app.messages.live_rows().count(),
        1,
        "latched: nothing folds"
    );
    app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_100)), false);
    assert_eq!(app.messages.live_rows().count(), 1);
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

/// The first run's end names how the child exited: Complete when clean,
/// Fault when not.
#[test]
fn the_first_run_end_echoes_complete_when_clean_and_fault_when_not() {
    for (clean, kind) in [(true, EchoKind::Complete), (false, EchoKind::Fault)] {
        let mut app = App::headless_for_test();
        app.announce_toolchain_pass("installing 10 ALab program(s)", true);
        app.apply_toolchain_snapshot(Some(&net_read(true, 1_700_000_000)), true);
        app.toolchain_pass_ended(clean);
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(
            app.messages.echoes().first().map(|e| e.kind),
            Some(kind),
            "clean={clean}"
        );
    }
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
    assert_eq!(
        failure_rows(&app).len(),
        1,
        "the live row until the child exits"
    );
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
    assert_eq!(failure_rows(&app)[0].1, Hold::Default);
    // A retry's first read replaces it at once — never behind the failure
    // it answers — and so does the next first run's announcement.
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
    app.announce_toolchain_pass("installing 10 ALab program(s) (about 3 GB)", true);
    assert!(
        failure_rows(&app)
            .iter()
            .all(|(_, hold, _)| matches!(hold, Hold::Live { .. })),
        "a new pass takes the key back"
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

    // A heavy ROUTINE pass that fails: the record only.
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

/// A headless App with its first window put ON SCREEN (the test seam for
/// a real OS window) and focused — the one place the band moves.
fn on_screen_app() -> App {
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
    ws.band_on_screen_for_test = true;
    ws.focused = true;
    app.system_reduce_motion = false;
    app.serious_mode = false;
    app
}

/// THE IDLE LAW (design §10.8): no row, nothing armed; a live row on a
/// window that is not on screen (headless, occluded), nothing armed; a
/// held row only, nothing armed; a live row on screen, a frame on the
/// grid; after the last row folds and its echo ends, nothing armed.
#[test]
fn the_band_motion_deadline_is_none_when_idle_or_hidden() {
    let mut app = App::headless_for_test();
    let now = Instant::now();
    assert_eq!(
        app.band_motion_deadline(now),
        None,
        "an idle band arms nothing"
    );
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    assert_eq!(app.message_band_rows, 1);
    assert_eq!(
        app.band_motion_deadline(now),
        None,
        "a headless window has no motion"
    );
    let mut app = on_screen_app();
    app.post_message(warn("a held warning"));
    assert_eq!(
        app.band_motion_deadline(now),
        None,
        "a held row never moves"
    );
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let now = Instant::now();
    let at = app
        .band_motion_deadline(now)
        .expect("a live row on screen moves");
    assert!(
        at > now && at <= now + aterm_messages::ANIM_FRAME * 2,
        "the next frame is on the grid"
    );
    app.windows.get_mut(&WindowId(0)).unwrap().occluded = true;
    assert_eq!(
        app.band_motion_deadline(now),
        None,
        "occluded: nothing armed"
    );
    app.windows.get_mut(&WindowId(0)).unwrap().occluded = false;
    app.toolchain_pass_ended(true);
    app.clear_messages_for_test();
    assert_eq!(
        app.band_motion_deadline(Instant::now()),
        None,
        "folded: nothing armed"
    );
}

/// Unfocused, the band is STILL: the meter is its still form and the
/// only wake left is a time word's tick — never a frame-rate wake.
#[test]
fn an_unfocused_window_paints_the_still_form_and_wakes_only_for_text() {
    let mut app = on_screen_app();
    app.windows.get_mut(&WindowId(0)).unwrap().focused = false;
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let look = app.band_look(WindowId(0));
    assert_eq!(look.pace, aterm_messages::Pace::Still);
    let now = Instant::now();
    let first = app
        .band_motion_deadline(now)
        .expect("the elapsed words tick");
    assert!(
        first >= now + std::time::Duration::from_secs(5),
        "no frame-rate wake while unfocused: {:?}",
        first.saturating_duration_since(now)
    );
    let fp0 = app.prepare_band_motion(WindowId(0), now);
    let fp1 = app.prepare_band_motion(WindowId(0), now + std::time::Duration::from_secs(3));
    assert_eq!(fp0, fp1, "the still form does not move with time");
    let motion = app.windows[&WindowId(0)]
        .band
        .motion()
        .cloned()
        .expect("prepared");
    assert_eq!(
        motion.rows[0].anim,
        aterm_messages::Anim::Track,
        "the still busy form: the unlit track"
    );
    assert!(
        motion.rows[0]
            .surface
            .cells(80)
            .iter()
            .all(|t| *t == aterm_messages::Tone::TRACK),
        "…lit nowhere"
    );
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

/// SERIOUS MODE AND REDUCED MOTION retire motion only: the band keeps
/// its row and its words, in the still form.
#[test]
fn serious_mode_and_reduced_motion_paint_the_still_forms() {
    let wid = WindowId(0);
    let mut app = on_screen_app();
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    assert_eq!(app.band_look(wid).pace, aterm_messages::Pace::Moving);
    app.serious_mode = true;
    assert_eq!(
        app.band_look(wid).pace,
        aterm_messages::Pace::Still,
        "Serious Mode"
    );
    assert_eq!(app.message_band_rows, 1, "…keeps the row");
    app.serious_mode = false;
    app.system_reduce_motion = true;
    assert_eq!(
        app.band_look(wid).pace,
        aterm_messages::Pace::Still,
        "Reduce Motion"
    );
    app.system_reduce_motion = false;
    app.config.motion = Some("reduced".into());
    assert_eq!(
        app.band_look(wid).pace,
        aterm_messages::Pace::Still,
        "motion = reduced"
    );
    app.config.motion = None;
    assert_eq!(app.band_look(wid).pace, aterm_messages::Pace::Moving);
}

/// AN ANIMATION NEVER RE-GRIDS: 200 motion frames and a Complete echo
/// cost exactly the open and the fold the same run costs with no motion.
#[test]
fn an_animation_never_regrids() {
    let wid = WindowId(0);
    let mut app = on_screen_app();
    let regrids = message_regrids();
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    assert_eq!(message_regrids(), regrids + 1, "the open");
    let mut t = Instant::now();
    let mut moved = std::collections::BTreeSet::new();
    for _ in 0..200 {
        t += aterm_messages::ANIM_FRAME;
        for due in app.band_motion_due(t) {
            moved.insert(app.prepare_band_motion(due, t));
        }
        assert_eq!(app.message_band_rows, 1);
    }
    assert!(
        moved.len() > 10,
        "the frames drew something new: {}",
        moved.len()
    );
    assert_eq!(message_regrids(), regrids + 1, "no frame re-grids");
    app.toolchain_pass_ended(true);
    for _ in 0..30 {
        t += aterm_messages::ANIM_FRAME;
        app.prepare_band_motion(wid, t);
    }
    assert_eq!(message_regrids(), regrids + 1, "the echo re-grids nothing");
    for _ in 0..40 {
        t += std::time::Duration::from_millis(100);
        app.settle_messages(t);
    }
    assert_eq!(app.message_band_rows, 0);
    assert_eq!(message_regrids(), regrids + 2, "one open, one fold");
}

// -----------------------------------------------------------------------
// The update lane on the band (the ux/status-reporting merge, 2026-09-23).
// -----------------------------------------------------------------------

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
/// DOES move the stamp, so the reading is not vacuous. And with no window
/// on screen (a headless App) there is no frame and no wake.
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
        app.automatic_update_activity_quiet_with_pending_input(later, false),
        quiet_before,
        "the automatic lane reads the terminal exactly as it would without the comet"
    );
    // Off screen (a headless window): no frame, and nothing wakes.
    let mut headless = App::headless_for_test();
    headless.note_update_progress(&aterm_update::Progress::Verifying {
        version: "9.9.9".into(),
    });
    assert_eq!(headless.band_motion_deadline(later), None);
    assert!(headless.band_motion_due(later).is_empty());
    assert_eq!(clocks(&app), before);
    // NEGATIVE CONTROL: real (tolerated) activity does move the stamp.
    std::thread::sleep(std::time::Duration::from_millis(2));
    app.note_update_handoff_tolerated_activity();
    assert_ne!(
        clocks(&app).0,
        before.0,
        "the clock this test reads is live"
    );
}

/// THE BAND ANIMATES ONLY WHILE A BUSY ROW IS COMMITTED, THE WINDOW IS
/// FOCUSED AND ON SCREEN, MOTION IS ALLOWED AND NOTHING IS FROZEN (the
/// gating union, ruling 140): serious mode and Reduce Motion draw the row
/// still; a busy row queued behind the committed rows does not animate;
/// and under a handoff freeze the band is still and arms and asks for
/// nothing — the frame on glass stays WHERE IT IS, not even a switch to
/// still repaints the parked screen.
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
    app.windows.get_mut(&wid).unwrap().focused = false;
    assert_eq!(app.band_look(wid).pace, Pace::Still, "no focused window");
    app.windows.get_mut(&wid).unwrap().focused = true;
    app.serious_mode = true;
    assert_eq!(app.band_look(wid).pace, Pace::Still, "serious mode: still");
    app.serious_mode = false;
    app.system_reduce_motion = true;
    assert_eq!(app.band_look(wid).pace, Pace::Still, "Reduce Motion: still");
    app.system_reduce_motion = false;
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

/// A FRAME NEVER BUMPS THE CENTER'S REVISION: a frame is paint, so
/// Settings ▸ Messages — which republishes on the revision — is never
/// republished at the frame rate.
#[test]
fn a_frame_tick_never_bumps_the_center_revision() {
    let mut app = on_screen_app();
    app.note_update_progress(&aterm_update::Progress::Verifying {
        version: "9.9.9".into(),
    });
    let revision = app.messages.revision();
    let now = Instant::now();
    for k in 0..40_u32 {
        app.prepare_band_motion(WindowId(0), now + aterm_messages::ANIM_FRAME * k);
    }
    assert_eq!(app.messages.revision(), revision);
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

/// UNSAVED EDITOR WORK HOLDING THE INSTALL IS A ROW OF ITS OWN: the one
/// blocker only the person can clear, painted (ruling 77) — the automatic
/// lane raises no row of its own to carry it (the owner's silent path,
/// 2026-09-24). Where the lane retries by itself the row STANDS (round eight,
/// lead U38: the wait has no deadline, and the overdue notice is off while it
/// holds); a press nothing retries keeps the transient outcome row.
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
        outcome.msg.title,
        crate::app_update_screen::UPDATE_WAITS_FOR_YOU
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
    // columns the instruction needs — at 80 it reads whole up to its last
    // word, and at 60 it still says what to do. (A standing row is born a
    // progress grace late.)
    let _ = app.settle_messages(
        Instant::now() + aterm_messages::PROGRESS_GRACE + std::time::Duration::from_millis(1),
    );
    for (cols, painted) in [
        (80, "Save or close the open editor to finish"),
        (60, "Save or close"),
    ] {
        let row = app.band_presentation(cols).rows[0].clone();
        let detail = row.detail.map(|(_, words)| words).unwrap_or_default();
        assert!(detail.starts_with(painted), "{cols} columns: {detail:?}");
        assert!(
            row.capsules.iter().all(|c| c.action.is_details()),
            "{cols} columns: only `Details ›`"
        );
    }
    // Unsaved Settings text alone, as the close preflight words it (the
    // row's words are the answer, not a reason): "Settings" survives the cut.
    let mut settings = App::headless_for_test();
    settings.note_update_blockers(
        &[format!(
            "{} 1 Settings view(s) have unsaved text",
            crate::app_native::SETTINGS_DRAFTS_BLOCK
        )],
        true,
    );
    assert_eq!(
        settings
            .messages
            .live_by_key(update_words::KEY_WAITS_FOR_YOU)
            .expect("the Settings wait's row")
            .msg
            .detail[0],
        App::UNSAVED_SETTINGS_BLOCKS_APPLY
    );
    assert_eq!(settings.messages.live_rows().count(), 1, "one row");
    let _ = settings.settle_messages(
        Instant::now() + aterm_messages::PROGRESS_GRACE + std::time::Duration::from_millis(1),
    );
    for (cols, painted) in [
        (80, "Save or close the unsaved Settings tabs"),
        (60, "Save or close"),
    ] {
        let row = settings.band_presentation(cols).rows[0].clone();
        let detail = row.detail.map(|(_, words)| words).unwrap_or_default();
        assert!(detail.starts_with(painted), "{cols} columns: {detail:?}");
    }
    // NEGATIVE CONTROL: a blocker that clears by itself raises nothing.
    let mut quiet = App::headless_for_test();
    quiet.note_update_blockers(&[App::RESTORE_IN_FLIGHT_BLOCKS_APPLY.to_string()], true);
    assert_eq!(quiet.messages.live_rows().count(), 0);
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

/// A HELD UPDATE ROW REPLACED BY A NEWER ONE IS RESOLVED ON RECORD (main's
/// rule, kept by ruling 143): one outcome row replaced by another leaves a
/// finished activity on `appstatus` — a superseded row would record
/// nothing. A failed download is a record of its own (a self-retry), and
/// the live download row it ends is resolved `Warn`, on record too.
#[test]
fn a_held_update_row_replaced_by_a_newer_one_is_resolved_on_record() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    // A person asked (Check for Updates): the download is their wait
    // and takes the row (design ruling 220).
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

/// THE LANDING IS ON RECORD WITH HOW LONG THE UPDATE TOOK — from THIS
/// build's finished download, carried across the handoff; never for a build
/// met already on disk, nor for another build's download.
#[test]
fn the_landing_is_on_record_with_how_long_the_update_took() {
    // One landing, one entry: the duration is a line of the landing
    // record, never a second record (audit 2026-09-24).
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
    assert!(
        app.messages
            .log()
            .records()
            .any(|r| r.title == "Updated to aterm v9.9.9"),
        "the landing's words are on record"
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

/// A HEALED WARNING IS RECORDED AS WORKING AGAIN — ONCE per announced
/// episode, after its 45 s fold too; a second heal records nothing.
#[test]
fn a_healed_update_warning_is_recorded_as_working_again_once() {
    let body = "3 failed checks in a row since 2026-09-14T22:04:36Z: manifests \
                cannot be downloaded.";
    let mut app = App::headless_for_test();
    assert!(app.note_update_health(aterm_update::health_failing_title("pipeline"), body));
    let now = Instant::now();
    let _ =
        app.settle_messages(now + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(1));
    assert!(app.messages.live_health().is_none(), "folded");
    let healed = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| r.title == "aterm updates work again")
            .count()
    };
    assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, ""));
    assert_eq!(healed(&app), 1);
    let rec = app
        .messages
        .log()
        .records()
        .rfind(|r| r.title == "aterm updates work again")
        .unwrap();
    assert_eq!(
        rec.detail[0],
        "after \"Couldn't download updates\", since Sep 14"
    );
    assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, ""));
    assert_eq!(healed(&app), 1, "once per episode");
}

/// A PERSISTENT FAILURE IS READ ONCE PER CLASS: its title names the broken
/// half, the excerpt since when, the whole sentence behind Details, the
/// warning's hold (never Standing); a count restatement posts nothing.
#[test]
fn a_persistent_failure_is_read_once_per_class() {
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: release manifests \
                exist but cannot be downloaded.";
    for class in ["apply", "pipeline", "stage", "manifest"] {
        let mut app = App::headless_for_test();
        let title = aterm_update::health_failing_title(class);
        assert!(app.note_update_health(title, body));
        let row = app.messages.live_health().expect("the warning");
        assert_eq!(row.msg.title, title);
        assert_eq!(row.msg.detail[0], "since Sep 14");
        assert!(
            row.msg.detail[1..]
                .join(" ")
                .contains("release manifests exist but cannot be downloaded.")
        );
        assert_eq!(row.msg.hold, Hold::Default);
        let revision = app.messages.revision();
        assert!(!app.note_update_health(aterm_update::HEALTH_RESTATED_TITLE, "21 failed"));
        assert_eq!(app.messages.revision(), revision, "RESTATED posts nothing");
    }
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

/// A WARNING SAID AGAIN IN NEW WORDS (its promised retry withdrawn): the
/// row up takes them in place, no second row; once it has folded, a
/// record in them follows, so Messages keeps no withdrawn promise.
#[test]
fn a_restated_health_warning_takes_its_new_words_where_it_stands() {
    let apply = aterm_update::health_failing_title("apply");
    let kind = update_words::HealthKind::of_updater_title(apply);
    let words = |app: &App| app.messages.live_health().map(|l| l.msg.detail.join(" "));
    let mut app = App::headless_for_test();
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

/// THE LEDGER'S HEAL PROVES THE CLASS IT NAMES, NOT EVERY WARNING: an
/// install warning the convergence or overdue notice raised is not
/// answered by the updater healing a download streak it had announced —
/// that read put "aterm updates work again" on record over a build still
/// stranded. The heal of the apply class, or a heal that names no class
/// (the updater before it named one), still answers it.
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
/// a download starting, downloading by a staged build, installing only by a
/// landing (or the ledger's own recovery); an unanswered warning stays up
/// and remembered for the proof that does answer it.
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
    let staged = aterm_update::Progress::Staged {
        version: "9.9.9".into(),
        build: 7,
    };
    let warning_up = |app: &App| app.messages.live_health().is_some();
    // Install: neither a download nor a staged build answers it.
    let mut install = App::headless_for_test();
    assert!(install.note_update_health(aterm_update::health_failing_title("apply"), body));
    install.note_update_progress(&download("9.9.9"));
    install.note_update_progress(&staged);
    assert!(
        warning_up(&install),
        "a download says nothing about installing"
    );
    assert_eq!(healed(&install), 0, "NEGATIVE: no false recovery on record");
    install.post_update_landed("9.9.9", 7, 0);
    assert!(!warning_up(&install));
    assert_eq!(healed(&install), 1, "the landing is the proof");
    // Download: a download starting does not answer it; a staged build does.
    let mut download_class = App::headless_for_test();
    assert!(download_class.note_update_health(aterm_update::health_failing_title("stage"), body));
    download_class.note_update_progress(&download("9.9.9"));
    assert!(warning_up(&download_class));
    assert_eq!(healed(&download_class), 0);
    download_class.note_update_progress(&staged);
    assert!(!warning_up(&download_class));
    assert_eq!(healed(&download_class), 1);
    // Check: a download starting answers it.
    let mut check = App::headless_for_test();
    assert!(check.note_update_health(aterm_update::health_failing_title("manifest"), body));
    check.note_update_progress(&download("9.9.9"));
    assert!(!warning_up(&check));
    assert_eq!(healed(&check), 1);
    // The ledger's own recovery answers every class.
    let mut ledger = App::headless_for_test();
    assert!(ledger.note_update_health(aterm_update::health_failing_title("apply"), body));
    assert!(!ledger.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, ""));
    assert_eq!(healed(&ledger), 1);
}

/// A CARRIED WARNING HEALED BY THE LANDING IS ON THE SUCCESSOR'S RECORD: the
/// parent `_exit`s at Commit and never learns its warning healed, so the
/// successor that resolved the carried row records the healing at its
/// landing — once — against what the parent's warning said.
#[test]
fn a_carried_warning_healed_by_the_landing_is_on_the_successors_record() {
    let mut parent = App::headless_for_test();
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    parent.post_message(update_words::installing("0.76.0"));
    assert!(parent.note_update_health(aterm_update::health_failing_title("apply"), body));
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("0.76.0"));
    assert!(
        successor.messages.live_health().is_none(),
        "resolved with the handoff"
    );
    successor.post_update_landed("0.76.0", 7, 0);
    let said: Vec<String> = successor
        .messages
        .log()
        .records()
        .filter(|r| r.title == "aterm updates work again")
        .map(|r| r.detail.join(" "))
        .collect();
    assert_eq!(
        said,
        vec!["after \"Couldn't install updates\", since Sep 14".to_string()]
    );
    // A plain restart carries nothing to heal.
    let mut plain = App::headless_for_test();
    plain.seed_carried_messages(&carry, None);
    plain.post_update_landed("0.76.0", 7, 0);
    assert!(
        plain
            .messages
            .log()
            .records()
            .all(|r| r.title != "aterm updates work again"),
        "no finishing successor: nothing this process said, so nothing to record"
    );
}

/// THE HEALING IS RECORDED ON THE AUTOMATIC LANE AND AFTER THE FOLD TOO
/// (round six, finding 54): what the parent's warning said crosses the
/// handoff as its own field, so the successor's landing records "aterm
/// updates work again" when no progress row was carried (the automatic
/// lane posts none) and when the warning row had already folded — and the
/// field survives the manifest's own encoding.
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
    // The automatic lane: no progress row, the warning still up.
    let mut parent = App::headless_for_test();
    assert!(parent.note_update_health(aterm_update::health_failing_title("apply"), body));
    let carry = through_the_wire(&window_carry(
        parent.message_band_rows,
        parent.carried_messages(),
    ));
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("0.76.0"));
    successor.post_update_landed("0.76.0", 7, 0);
    assert_eq!(
        healed(&successor),
        vec!["after \"Couldn't install updates\", since Sep 14".to_string()]
    );
    assert!(successor.messages.live_health().is_none());
    // The warning folded before the carry: an explicit row, no warning row.
    let mut parent = App::headless_for_test();
    parent.post_message(update_words::installing("0.76.0"));
    assert!(parent.note_update_health(aterm_update::health_failing_title("apply"), body));
    for id in parent.live_update_health(|_| true) {
        parent.resolve_message(id, Outcome::Warn);
    }
    let carry = through_the_wire(&window_carry(
        parent.message_band_rows,
        parent.carried_messages(),
    ));
    assert!(!carry.update_health_said.is_empty());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("0.76.0"));
    successor.post_update_landed("0.76.0", 7, 0);
    assert_eq!(healed(&successor).len(), 1, "once");
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

/// THE HEAL IS KEYED BY KIND, AND AN OLDER BUILD'S WORDS STILL HEAL (design
/// ruling 311). The health titles took the family grammar (`Couldn't
/// download updates`), so nothing may match them word for word any more:
/// the row wears its kind's key, and the heal proof, the latch and the
/// record read that. A row an older build raised under the family key, in
/// its own words (`aterm can't download updates`, carried across a plain
/// restart), is read back to its kind — healed by the proof that answers
/// it and by no weaker one, and replaced when this build says the same
/// warning again.
///
/// NEGATIVE CONTROLS: the carried download row is NOT healed by a download
/// merely starting (the least proof, which is where an unread title would
/// fall); an install-KEYED row worded like a download is healed only by a
/// landing — the key, not the words, decides; and a stall row an older
/// build carried heals on a completed check, not by a download proof it
/// does not need.
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
    // An older build's row: the family key, the words before ruling 311.
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

    // THE KEY DECIDES, NOT THE WORDS: an install warning worded like a
    // download is not healed by a staged build.
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

/// TWO KINDS LIVE, TWO RECORDS OF HEALING (design ruling 318, completing
/// 311). Two halves failing in one launch are two rows, so each keeps its
/// own words for its own record: a later kind never takes an earlier one's
/// record of healing with it, and a proof that answers both writes both.
///
/// NEGATIVE CONTROLS: after Download then Install, a staged build records
/// the download's healing and NOT the install's (whose row stays up); the
/// watchdog's stall heal records only the stall, never a ledger warning
/// beside it; and each record is written once — a second landing adds
/// nothing.
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

    // Install, then Download: the landing answers the install only
    // (round-seven update audit, finding 54 — the build may have been
    // staged before the download streak began, and the ledger keeps it);
    // a staged build then answers the download. Both are on record, each
    // against its own words — not only the later, lesser warning.
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

    // Carried across the handoff: the successor seeds one entry per carried
    // row, so its landing records the install warning's healing and its
    // staged build the download's — both of the parent's warnings, each
    // once, each by the proof that answers it (finding 54).
    let mut parent = App::headless_for_test();
    parent.post_message(update_words::installing("9.9.9"));
    assert!(parent.note_update_health(download, body));
    assert!(parent.note_update_health(install, body));
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.post_update_landed("9.9.9", 7, 0);
    assert_eq!(
        healed(&successor),
        vec![after(install)],
        "NEGATIVE: the landing is no proof that downloading works"
    );
    successor.note_update_progress(&staged);
    let mut said = healed(&successor);
    said.sort();
    let mut want = vec![after(download), after(install)];
    want.sort();
    assert_eq!(said, want, "both of the parent's warnings, each once");
}

/// EVERY UPDATE MOMENT IS A LINE IN THE LOG (the log area's P3): downloaded,
/// the switch begun, a stopped automatic attempt, the landing — each a record
/// or a supersede the log keeps, in order, in its words; a restatement alone
/// writes no line, which is why the lane POSTS each phase.
#[test]
fn every_update_moment_is_a_line_in_the_log() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    // A person asked (Check for Updates): the download is their wait
    // and takes the row (design ruling 220).
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
    let titles: Vec<String> = app
        .messages
        .log()
        .records()
        .map(|r| r.title.clone())
        .collect();
    let pos = |needle: &str| {
        titles
            .iter()
            .position(|t| t.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} is not on record: {titles:#?}"))
    };
    assert!(pos("aterm v9.9.9 is ready") < pos("Updated to aterm v9.9.9"));
    // ONE SPELLING of the version on every update entry (audit 2026-09-24).
    for t in &titles {
        assert!(!t.contains("aterm 9.9"), "a bare version: {t:?}");
    }

    for phase in [
        "Downloading aterm v9.9.9",
        "Checking aterm v9.9.9",
        "aterm v9.9.9 is ready",
    ] {
        assert!(
            titles.iter().any(|t| t == phase),
            "downloading, checking and staged each a line (a title per phase, \
             ruling 143): {phase:?} in {titles:#?}"
        );
    }
    // NEGATIVE CONTROL: a restatement writes no line.
    let before = app.messages.log().len();
    let id = post_installing(&mut app, "9.9.8");
    let after_post = app.messages.log().len();
    assert!(after_post > before);
    app.restate_message(id, restatement_of(&update_words::finishing("9.9.8")));
    assert_eq!(
        app.messages.log().len(),
        after_post,
        "a restate is not a line"
    );
}

/// ONE FACT, ONE ENTRY at the switch (design ruling 213): an attempt
/// that POSTED its own Installing row has that line in the log already,
/// so the switch record is not written beside it; with no row up (the
/// automatic lane) the record is the attempt's only line.
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

/// A HARNESS NOTE IS RECORDED AND NEVER TAKES A ROW (design ruling 61): on
/// `appstatus` as `kind=harness`, no row, no re-grid, no wake; the same
/// note again is not written again; beside a first-run row it leaves that
/// one row alone.
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
    let last = appstatus(&app).pop().expect("on record");
    assert!(
        last.starts_with(
            "activity kind=harness phase=done progress=- \
             title=aterm%20harness%20acting%20%C2%B7%20approving%20rm "
        ),
        "{last}"
    );
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
    assert_eq!(
        app.messages.live_rows().next().unwrap().msg.title,
        toolchain_words::INSTALLING_ALAB
    );
}

/// AN UNCHANGED MANAGED-CURRENT MARKER IS RECORDED ONCE; NEWS IS RECORDED
/// AGAIN (design ruling 58): atpkg prints it every pass, and the shared ring
/// is for news — a moved version, or a frozen tab counted, is new words.
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
        identity: None,
        topics: Vec::new(),
        rekey: false,
        loader: false,
        repaint: false,
        fg_holder: 0,
        history: crate::handoff_history::AdoptedHistory::default(),
        hold: None,
        title: String::new(),
        supervisor: None,
        attention_owners: Vec::new(),
        claim_grace: false,
        outgoing_build: None,
        keeper: None,
        render_debt: None,
        fabric: Default::default(),
        timeline_id: 0,
        journal_key: None,
    }];
    assert!(
        app.post_managed_current(
            "claude 2.1.281 (Anthropic latest); codex 0.156.0 (OpenAI latest)"
        ),
        "a frozen tab counted is new words"
    );
    assert!(
        !app.post_managed_current(""),
        "an empty body records nothing"
    );
}

// ---- the wire's verbs: `notice` and `messages` (design rulings 163-179) ----

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

/// A warn post round-trips: the reply names the row, the band commits it,
/// and `messages` is the engine's own rows byte for byte (the host adds
/// nothing but the clock and the socket's encoder).
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
    let q = wire::ReadQuery::default();
    // `ago_ms=` reads the clock: retry across a millisecond boundary.
    let same = (0..5).any(|_| {
        let host = app.read_messages(&q);
        let engine = wire::message_rows(
            &app.messages,
            &q,
            wall_stamp_now().unix_ms,
            &crate::control::pct_encode,
        );
        host == engine
    });
    assert!(same, "{:?}", app.read_messages(&q));
    let rows = app.read_messages(&q);
    let row = rows.last().expect("one row");
    assert!(
        row.contains(" origin=wire state=held ")
            && row.contains(" key=wire.deploy ")
            && row.contains(" detail=connection%20refused "),
        "{row}"
    );
    // An info post is a record: no row, nothing on the band.
    let reply = notice(&mut app, "post script hello from a script");
    assert!(reply.starts_with("OK recorded message="), "{reply}");
    let rec = replied_id(&reply, "message=");
    assert!(app.messages.live(rec).is_none());
    assert_eq!(app.message_band_rows, 1);
}

/// A progress flood repaints at most once a motion frame: the engine paces,
/// the host counts only the paints it asked for at once, and the one paced
/// paint left over is in the loop's deadline and taken by its settle.
#[test]
fn a_wire_progress_flood_repaints_at_most_once_a_frame() {
    let mut app = App::headless_for_test();
    let before = WIRE_SYNCS.with(std::cell::Cell::get);
    let start = Instant::now();
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
    let elapsed_ms = start.elapsed().as_millis();
    let syncs = u128::from(WIRE_SYNCS.with(std::cell::Cell::get) - before);
    let frame_ms = aterm_messages::WIRE_PAINT_GAP.as_millis();
    assert!(
        syncs <= 2 + elapsed_ms / frame_ms,
        "{syncs} repaints in {elapsed_ms} ms"
    );
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

/// RULING 433's HELP NAMES WHAT THE WIRE REFUSES: `notice`'s help lists
/// aterm's own families by hand (aterm-types cannot see aterm-messages),
/// and here both are in view — the list is `tags::ALL` but `script`, the
/// set `wire::NoticeRequest::parse` refuses, no more and no fewer.
#[test]
fn the_notice_help_names_every_tag_the_wire_refuses() {
    use std::collections::BTreeSet;
    let help = aterm_types::control_verbs::spec("notice")
        .expect("the notice verb")
        .help_line();
    let listed: BTreeSet<&str> = help
        .split_once("since aterm's own (")
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(list, _)| list.split(", ").collect())
        .expect("the help lists aterm's own tags");
    let refused: BTreeSet<&str> = tags::ALL
        .iter()
        .filter(|tag| ***tag != tags::SCRIPT)
        .map(|tag| tag.as_str())
        .filter(|tag| {
            aterm_messages::wire::NoticeRequest::parse(&format!("post {tag} a note")).is_err()
        })
        .collect();
    assert_eq!(refused.len(), tags::ALL.len() - 1, "{refused:?}");
    assert_eq!(listed, refused, "{help}");
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

/// The host never spells the wire's key namespace: only `notice` can
/// name a `wire.` key, so no host row can ever be restated, ended or
/// superseded from the socket (design ruling 167).
#[test]
fn no_host_key_is_in_the_wire_namespace() {
    let needle = format!("\"{}", aterm_messages::WIRE_KEY_PREFIX);
    for (file, text) in [
        ("message_reporters.rs", include_str!("message_reporters.rs")),
        ("messages_host.rs", include_str!("messages_host.rs")),
        ("toolchain_words.rs", include_str!("toolchain_words.rs")),
        ("update_words.rs", include_str!("update_words.rs")),
        ("app_update_screen.rs", include_str!("app_update_screen.rs")),
        ("lib.rs", include_str!("lib.rs")),
    ] {
        assert!(!text.contains(&needle), "{file} spells a wire key");
    }
}

// ---- round 44: the second adversarial hunt's host repros -------------
//
// Each states the EXPECTED behaviour its candidate names and failed on
// the code it was written against; the fix stage makes each pass.

#[cfg(unix)]
/// host-msgs-R44-HOST-1 — on the AUTOMATIC lane no progress row is
/// carried; the parent's unanswered set (`update_health_said`) still is,
/// and the successor must not announce the same failing class again (a
/// second row). Round 44 kept that promise with a latch only a proof
/// lifts, which stood for the launch and muted the successor's own next
/// episode (round eight, findings 15 and 301); it is kept now by the
/// producer: the class is handed to this process's updater as announced
/// (`aterm_update::adopt_announced_health`, whose own test proves a streak
/// still failing is restated, never announced a second time), and the
/// window takes a restatement quietly.
#[test]
fn r44_host1_an_automatic_landing_hands_a_carried_check_class_to_the_updater() {
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    let mut parent = App::headless_for_test();
    assert!(parent.note_update_health_as(
        update_words::HealthKind::Check,
        "Couldn't check for updates",
        body,
    ));
    // No installing row: the automatic lane.
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    assert_eq!(
        carry.update_health_said.len(),
        1,
        "the parent's said crosses"
    );
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.messages.after_handoff_commit(Instant::now());
    successor.post_update_landed("9.9.9", 7, 0);
    assert_eq!(
        UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow().clone()),
        vec!["manifest"],
        "the successor's updater does not announce the carried class again"
    );
    assert!(
        !successor.note_update_health(aterm_update::HEALTH_RESTATED_TITLE, body),
        "its restatement is a log line"
    );
    assert_eq!(
        successor.live_update_health(|_| true).len(),
        1,
        "the carried row, never a second"
    );
}

#[cfg(unix)]
/// host-msgs-R44-HOST-1, the explicit lane: the check warning's row
/// folded in the parent (its 45 s hold) before the park, so no carried
/// row names it — but the parent's said does, and the landing does not
/// answer a check warning. Still handed to the successor's updater, once.
#[test]
fn r44_host1_a_folded_check_warning_is_handed_to_the_updater_across_the_landing() {
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
    let mut parent = App::headless_for_test();
    assert!(parent.note_update_health_as(
        update_words::HealthKind::Check,
        "Couldn't check for updates",
        body,
    ));
    let past = Instant::now() + aterm_messages::HOLD_WARN + std::time::Duration::from_secs(60);
    parent.settle_messages(past);
    assert!(
        parent.live_update_health(|_| true).is_empty(),
        "the warning folded in the parent"
    );
    parent.post_message(update_words::installing("9.9.9"));
    let carry = window_carry(parent.message_band_rows, parent.carried_messages());
    let mut successor = App::headless_for_test();
    successor.seed_carried_messages(&carry, Some("9.9.9"));
    successor.messages.after_handoff_commit(Instant::now());
    successor.post_update_landed("9.9.9", 7, 0);
    assert_eq!(
        UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow().clone()),
        vec!["manifest"],
        "the successor's updater does not announce the folded class again"
    );
}

/// host-msgs-R44-HOST-2 — ruling 28 and C10 (§10.3): a replace resolves
/// only what the reload re-derived, and `config.font-family` (the backend
/// thread's own verdict, its own key) is re-derived by no reload. A clean
/// full reload that did no font work must leave the rejected family's row
/// up, never close it `Font applied` ✓.
#[test]
fn r44_host2_a_reload_that_never_rederived_the_font_family_leaves_its_row() {
    let mut app = App::headless_for_test();
    let msg = crate::message_reporters::font_family_rejected(
        "font_family: \"Nope\" is not an admissible font",
    );
    let title = msg.title.clone();
    let id = app.post_message(msg);
    assert!(app.messages.live(id).is_some());
    app.replace_config_messages(Vec::new());
    let rec = app.messages.log().get(id).unwrap();
    assert!(
        app.messages.live(id).is_some(),
        "the rejected family's row came down on a reload that never re-derived it: \
         {:?} {:?} {:?}",
        rec.title,
        rec.severity,
        rec.state
    );
    assert_eq!(rec.title, title);
}

/// host-msgs-R44-HOST-2, the folded half — ruling 367 closes a closed
/// problem as fixed only when the reload's family is found fixed: the
/// family's row the person dismissed keeps its title and its ⚠ through a
/// reload that never re-derived it.
#[test]
fn r44_host2_a_folded_font_family_record_is_not_closed_fixed_by_a_reload() {
    let mut app = App::headless_for_test();
    let msg = crate::message_reporters::font_family_rejected(
        "font_family: \"Nope\" is not an admissible font",
    );
    let title = msg.title.clone();
    let id = app.post_message(msg);
    assert!(app.messages.dismiss(id, Instant::now()));
    app.sync_messages();
    app.replace_config_messages(Vec::new());
    let rec = app.messages.log().get(id).unwrap();
    assert_eq!(
        (rec.title.as_str(), rec.severity),
        (title.as_str(), Severity::Warn),
        "{rec:?}"
    );
}

/// Round 44's review of HOST-2 (ruling 417 (i)): a reload whose FONT WORK
/// ran does re-derive the family — the catalog worker admits it, or says
/// `config font_family: …` under `config.fonts` — so one that ADMITTED it
/// (the owner cleared the family) answers the launch thread's rejection:
/// its row closes fixed, as a reload closed it before round 44. One that
/// rejected the family again leaves the row: its problem stands.
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
/// page's and the wire's rule). A stale page press on a retired strain
/// row's `Show tab 2` is `Gone`: no tab switched, nothing raised.
#[test]
fn r44_host3_a_retired_rows_show_tab_is_gone_from_the_page() {
    let mut app = App::headless_for_test();
    let a = WindowId(0);
    app.push_stub_tab(a, crate::stub_session(app.next_session_id));
    app.switch_tab_in(a, 0);
    let active = |app: &App| app.windows[&a].tab_set.active_index();
    assert_eq!(active(&app), Some(0));
    let id = app.post_message(
        Message::new(
            aterm_messages::tags::SYSTEM,
            Severity::Warn,
            "Typing slowed by 'yes' (tab 2)",
        )
        .action(Intent::ShowTab {
            tab: 2,
            window: a.0,
        }),
    );
    assert!(app.withdraw_message(id));
    assert!(app.messages.live(id).is_none(), "retired");
    let rec = app.messages.log().get(id).unwrap();
    assert!(
        !rec.still_offers(&rec.actions[0]),
        "the engine's rule: no longer offered"
    );
    assert_eq!(
        app.perform_message_act(a, id.raw(), 0),
        MessageActOutcome::Gone,
        "a retired row's Show tab was performed from the page"
    );
    assert_eq!(active(&app), Some(0), "no tab switched");
}

/// host-msgs-R44-HOST-3 — the same rule for `Stop paste` and `End
/// sessions`: the host refuses what the record no longer offers.
#[test]
fn r44_host3_a_retired_rows_row_ending_intents_are_gone_from_the_page() {
    for intent in [Intent::StopPaste { session: 1 }, Intent::EndRecovered] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let id = app.post_message(
            Message::new(
                aterm_messages::tags::SESSION,
                Severity::Info,
                "Pasting 4.2 MB",
            )
            .action(intent.clone()),
        );
        assert!(app.withdraw_message(id));
        assert_eq!(
            app.perform_message_act(wid, id.raw(), 0),
            MessageActOutcome::Gone,
            "{intent:?}"
        );
    }
}
