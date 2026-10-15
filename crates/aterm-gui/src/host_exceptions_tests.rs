// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S OWN FACTS AS EXCEPTIONS (owner rules, 2026-09-28: an alert
//! persists only where the owner can act, and it says why and what; a fact that
//! mends itself is a record, and a line of "To look at" only once it has
//! outlasted its horizon; full-automatic — nothing here blocks on a person).
//!
//! What is checked, per producer: the words pass the engine's door (90/90
//! caps, one action, no waiting word, no restart word), the fact is a record
//! when it begins and a digest line only when it goes on, its end takes the
//! line away, and none of it marks a tab, seats a band row or re-grids a
//! window. The roster covers every entry the host can write.

use std::time::{Duration, Instant};

use aterm_agent::supervise::StuckSurvey;
use aterm_agent::supervise::policy::exception::{Exception, Urgency};
use aterm_agent::supervise::policy::outlook::ExceptionKind;
use aterm_core::terminal::repaint_nudge::{Gate, GiveUp};
use aterm_messages::exceptions::{Entry, Family};

use crate::App;
use crate::app_presence::presence_regrids;
use crate::harness_host::{AccountsSaid, HostNote, RelaunchSay};
use crate::message_reporters as mr;
use crate::messages_host::message_regrids;
use crate::update_words::{self, HealthKind};

const DAY: Duration = Duration::from_secs(24 * 3600);

/// The places a tab can be named in: alone, in one of several windows, unknown.
fn places() -> [mr::UpgradeSpot; 3] {
    [
        mr::UpgradeSpot {
            place: "in tab 3".to_string(),
            nav: Some(aterm_messages::Intent::ShowTab { tab: 3, window: 1 }),
        },
        mr::UpgradeSpot {
            place: "in window 12, tab 10".to_string(),
            nav: Some(aterm_messages::Intent::ShowTab {
                tab: 10,
                window: 12,
            }),
        },
        mr::UpgradeSpot {
            place: "in its tab".to_string(),
            nav: None,
        },
    ]
}

/// Every entry the host can write, in every place it can be worded for.
fn roster() -> Vec<Entry> {
    let body = "3 failed checks in a row since 2026-09-14T22:04:36Z: manifests cannot be \
                downloaded.";
    let mut all: Vec<Entry> = HealthKind::ALL
        .into_iter()
        .map(|kind| update_words::update_health_entry(kind, body))
        .collect();
    all.push(update_words::update_health_entry(HealthKind::Install, ""));
    for spot in places() {
        all.push(mr::supervisor_failing_entry(
            "s-3e79b5bde172ad0088ad",
            &spot,
            "boom",
        ));
        all.push(mr::survey_entry("s-3e79", &spot, "a rating card is open"));
        for why in [
            GiveUp::NoAnswer,
            GiveUp::Deferred(Gate::Busy),
            GiveUp::Deferred(Gate::Selection),
            GiveUp::Deferred(Gate::Lease),
            GiveUp::Budget,
        ] {
            all.push(mr::render_unrepaired_entry("s-3e79", &spot, why));
        }
    }
    all.push(mr::disk_low_entry(7, 20, &[]));
    // The foreign tools atpkg leaves alone, each look's: one, and more than the
    // entry shows.
    let foreign = |n: usize| {
        (0..n)
            .map(|i| crate::toolchain_words::Foreign {
                name: "trust".to_string(),
                path: format!("/Users//someone/.rustup/toolchains/trust-{i}"),
                fix: atpkg::seam::DETACH_FIX.to_string(),
            })
            .collect::<Vec<_>>()
    };
    for scope in [atpkg::cli::FOREIGN_SEAM, atpkg::cli::FOREIGN_PATH] {
        for n in [1, 5] {
            all.push(crate::toolchain_words::foreign_tools_entry(
                scope,
                &foreign(n),
            ));
        }
    }
    all.push(mr::disk_low_cleanup_off_entry(2, 10));
    all.push(mr::disk_low_cleanup_off_entry(
        u64::MAX >> 30,
        u64::MAX >> 30,
    ));
    // The disk settings': one note, and more than the entry shows.
    let note = format!(
        "/Users//someone/{}/aterm.toml: cannot be read (Permission denied (os error 13)), so \
         every removal is off (disk.apply false, auto_free_gib 0, dead_build_dirs false) until \
         it is fixed",
        "x".repeat(40)
    );
    all.push(mr::disk_settings_entry(std::slice::from_ref(&note)));
    all.push(mr::disk_settings_entry(
        &std::iter::repeat_n(note, 5).collect::<Vec<_>>(),
    ));
    all.push(mr::upgrade_cannot_run_entry("busy:state-unwritable", None));
    all.push(mr::devtools_entry());
    // The accounts': one signed out, three, the most a roster lists at the
    // longest a label can be, the longest label the action names and the first
    // it does not, and the roster that is refused.
    for labels in [
        vec!["work-2".to_string()],
        ["main", "work-2", "pool-a"].map(str::to_string).to_vec(),
        (0..32)
            .map(|i| format!("{i:02}{}", "x".repeat(62)))
            .collect(),
        vec!["a".repeat(57)],
        vec!["a".repeat(58)],
    ] {
        all.push(mr::accounts_signed_out_entry(&labels).expect("a label is an entry"));
    }
    all.push(mr::accounts_refused_entry());
    // The relaunch's, in every place, for every cause and reason it can name.
    for spot in places() {
        for cause in ["held", "shell-prompt", "other-tab"] {
            all.push(mr::relaunch_blocked_entry("s-3e79", &spot, cause, false));
        }
        for reason in ["its shell is gone", "it ran one prompt and ended"] {
            all.push(mr::relaunch_gone_entry("s-3e79", &spot, reason, false));
            all.push(mr::relaunch_gone_entry("s-3e79", &spot, reason, true));
        }
    }
    // The resume loop's: every kind, alone and in three tabs, in every place.
    for spot in places() {
        let one = vec![("s-1".to_string(), stall_exception(), spot.clone())];
        all.extend(mr::resume_exceptions(&one));
        for kind in later_kinds() {
            let many: Vec<_> = ["s-1", "s-2", "s-3"]
                .iter()
                .map(|sid| (sid.to_string(), resume_exception(kind), spot.clone()))
                .collect();
            all.extend(mr::resume_exceptions(&many));
            all.extend(mr::resume_exceptions(&many[..1]));
        }
    }
    all
}

/// The kinds of the resume loop's exceptions that wait for the owner's look.
fn later_kinds() -> [ExceptionKind; 6] {
    [
        ExceptionKind::Spend,
        ExceptionKind::Bucket,
        ExceptionKind::GoalCleared,
        ExceptionKind::Futile,
        ExceptionKind::Context,
        ExceptionKind::Dialog,
    ]
}

/// The loop's own words for `kind` (`policy/exception.rs`), as it tells them.
fn resume_exception(kind: ExceptionKind) -> Exception {
    let (why, action) = match kind {
        ExceptionKind::Spend => (
            "Its usage credits are used up, and no time window ends that.",
            "Add usage credits in claude.ai settings.",
        ),
        ExceptionKind::Bucket => (
            "Fable has no reset time, and this tab has no other model to fall back on.",
            "Run /model in this tab and pick another model.",
        ),
        ExceptionKind::GoalCleared => (
            "Claude cleared the /goal: organization verification required.",
            "Run /goal again in this tab.",
        ),
        ExceptionKind::Futile => (
            "The limit still stands an hour after its reset.",
            "Check claude.ai/settings/usage for what holds it.",
        ),
        ExceptionKind::Stalled => return stall_exception(),
        ExceptionKind::Dialog => return Exception::dialog(),
        ExceptionKind::Context => return Exception::context(),
        ExceptionKind::Login => (
            "Its login is gone, and only a browser sign-in restores it.",
            "Finish signing in on the page /login opened in your browser.",
        ),
    };
    Exception {
        kind,
        when: if kind == ExceptionKind::Login {
            Urgency::Now
        } else {
            Urgency::Later
        },
        why: why.to_string(),
        action: action.to_string(),
        clears: "the agent working",
    }
}

/// The stall canary's third rung, as the loop tells it.
fn stall_exception() -> Exception {
    Exception::stalled()
}

/// EVERY WORD THE HOST CAN SAY passes the door and the rule's own caps: a title
/// that is a glass title, a reason and an action of at most 90 characters, one
/// action (no `; `, no ` or `), a reason that does not lean on waiting, and no
/// word that asks for a restart (grep_guard B12). Keys stay within their cap, sit
/// under their family's prefix, and share nothing with a record's key.
#[test]
fn every_host_entry_passes_the_door_and_the_caps() {
    let record_keys: Vec<String> = HealthKind::ALL
        .into_iter()
        .map(|kind| kind.key().to_string())
        .chain([
            mr::supervisor_failing_record("s-1", "in tab 1", "x")
                .key
                .unwrap_or_default(),
            mr::disk_low_record(1, 2, &[]).key.unwrap_or_default(),
            mr::disk_low_cleanup_off_record(1, 2)
                .key
                .unwrap_or_default(),
        ])
        .collect();
    for entry in roster() {
        let msg = entry
            .message()
            .unwrap_or_else(|fault| panic!("{}: {fault:?}", entry.key()));
        let aterm_messages::need::Need::Later(ask) = msg.need() else {
            panic!("{}: an entry is a Later ask", entry.key());
        };
        let (why, then) = (ask.why_words(), ask.then().words().to_string());
        assert!(why.chars().count() <= 90 + 24, "{}: {why}", entry.key());
        assert!(then.chars().count() <= 90, "{}: {then}", entry.key());
        assert!(!then.contains("; ") && !then.contains(" or "), "{then}");
        for line in [&why, &then, &msg.title] {
            let low = line.to_lowercase();
            // Spelled in pieces: grep_guard B12 reads this file for the same words.
            let next_launch = ["next", "launch"].join(" ");
            let cold = ["re", "boot"].concat();
            for needle in ["restart", "relaunch", "reopen", &next_launch, &cold] {
                assert!(!low.contains(needle), "{}: {line}", entry.key());
            }
        }
        let key = entry.key();
        assert!(key.chars().count() <= 48, "{key}");
        assert!(key.starts_with(&entry.family().prefix()), "{key}");
        assert!(
            !record_keys.contains(&key),
            "{key}: an entry and a record never share a key"
        );
    }
}

/// One producer, one mode: the families this commit adds or uses speak by
/// event, and the host refuses the wrong call.
#[test]
fn the_host_families_speak_by_event() {
    use aterm_messages::exceptions::Mode;
    for family in [
        Family::Update,
        Family::Supervisor,
        Family::Accounts,
        Family::Disk,
        Family::UpgradeHost,
        Family::Render,
    ] {
        assert_eq!(family.mode(), Mode::Event, "{family:?}");
    }
    assert_eq!(Family::Render.stem(), "render");
    assert_eq!(Family::Render.tag(), aterm_messages::tags::RENDER);
    assert!(Family::ALL.contains(&Family::Render));
}

/// A FACT THAT MENDS ITSELF IS AN ENTRY ONLY ONCE IT HAS OUTLASTED ITS HORIZON:
/// `except_after` holds the claim, raises it at its instant (the loop is told
/// when: `messages_deadline`), and an end before then withdraws it with no line
/// ever shown. NEGATIVE CONTROLS: the same fact re-asserted does not postpone
/// itself, and a zero horizon is an entry at once.
#[test]
fn a_claim_ripens_at_its_horizon_and_an_end_withdraws_it() {
    let mut app = App::headless_for_test();
    let entry = update_words::update_health_entry(HealthKind::Download, "");
    app.except_after(&entry, Duration::from_secs(3600));
    assert!(app.messages.digest().is_empty(), "not yet");
    let due = app.ripening_deadline().expect("the loop is told when");
    assert!(app.messages_deadline().is_some_and(|at| at <= due));
    // Said again 30 minutes on: the instant does not move.
    app.except_after(&entry, Duration::from_secs(3600));
    assert_eq!(app.ripening_deadline(), Some(due));
    let _ = app.settle_messages(due - Duration::from_secs(1));
    assert!(app.messages.digest().is_empty(), "a second early");
    let _ = app.settle_messages(due);
    assert_eq!(app.messages.digest().len(), 1);
    assert!(app.ripening_deadline().is_none(), "raised: no claim left");
    // An end before the horizon shows nothing, ever.
    let mut app = App::headless_for_test();
    app.except_after(&entry, Duration::from_secs(3600));
    app.except_clear(&entry.key());
    assert!(app.ripening_deadline().is_none());
    let _ = app.settle_messages(Instant::now() + 2 * DAY);
    assert!(app.messages.digest().is_empty());
    // No horizon: at once.
    let mut app = App::headless_for_test();
    app.except_after(&entry, Duration::ZERO);
    assert_eq!(app.messages.digest().len(), 1);
    // A LEVEL family is refused, as `except` refuses it.
    let level = Entry::new(
        Family::Upgrade,
        "",
        "own-work",
        "Claude is behind",
        aterm_messages::need::Why::OnlyYou(aterm_messages::need::Reason::new("it is busy")),
        aterm_messages::need::Then::step("End that job in the tab"),
    );
    app.except_after(&level, Duration::from_secs(60));
    assert!(app.ripening_deadline().is_none());
}

/// UPDATE HEALTH: a failing kind is a warning that folds into its record now,
/// and one line of the digest (`update.review.<kind>`, never the record's
/// `update.health.<kind>`) once the ledger's own streak has gone on a day —
/// counted from the streak's start the body dates, so a process that begins on
/// day six says it at once and a restart never begins the day again. The
/// healing ends the line. NEGATIVE CONTROL: another kind's healing leaves it.
#[test]
fn updates_failing_for_a_day_are_one_line_and_the_healing_ends_it() {
    let stamp = |ago: u64| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        aterm_types::rfc3339::format_rfc3339(now - ago)
    };
    let body = |ago: u64| {
        format!(
            "20 failed checks in a row since {}: release manifests exist but cannot be \
             downloaded.",
            stamp(ago)
        )
    };
    let title = aterm_update::health_failing_title("pipeline");
    // An hour into the streak: a claim for the rest of the day, no line.
    let mut app = App::headless_for_test();
    assert!(app.note_update_health(title, &body(3600)));
    assert!(app.messages.digest().is_empty());
    let due = app.ripening_deadline().expect("a claim");
    let left = due.duration_since(Instant::now());
    assert!(
        left > Duration::from_secs(22 * 3600) && left <= Duration::from_secs(23 * 3600),
        "{left:?}"
    );
    let _ = app.settle_messages(due);
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, ["update.review.download"]);
    // A proof that only the check works leaves a download line; the proof of a
    // staged build, or a healed ledger, ends it.
    app.heal_update_health(crate::messages_host::HealthProof::Checked);
    assert_eq!(
        app.messages.digest().len(),
        1,
        "a check that works downloads nothing"
    );
    app.heal_update_health(crate::messages_host::HealthProof::Downloaded);
    assert!(
        app.messages.digest().is_empty(),
        "downloading works: the line ends"
    );
    // And a healed ledger ends a line of any kind but the stall and the sibling's lock.
    let mut app = App::headless_for_test();
    assert!(app.note_update_health(title, &body(6 * 24 * 3600)));
    assert_eq!(app.messages.digest().len(), 1);
    assert!(!app.note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, ""));
    assert!(app.messages.digest().is_empty(), "the ledger healed");
    // An established streak keeps its original date, regardless of the month
    // this test runs in. The relative six-day case above checks the age gate.
    let historical_body = "20 failed checks in a row since 2026-09-14T22:04:36Z: release \
                           manifests exist but cannot be downloaded.";
    let mut app = App::headless_for_test();
    assert!(app.note_update_health(title, historical_body));
    assert_eq!(app.messages.digest().len(), 1);
    assert!(app.ripening_deadline().is_none());
    // The warning row is not the entry: three sources of one fact, one line.
    let e = &app.messages.digest().entries[0];
    assert_eq!(e.title, "Updates are not downloading");
    assert!(
        e.detail.iter().any(|l| l == "failing since Sep 14"),
        "{:?}",
        e.detail
    );
}

/// `health_entry_delay` is the day less what the streak has already spent, never
/// negative and never more than a day: a body with no readable start counts from
/// now.
#[test]
fn the_entry_delay_is_the_day_less_the_streak_so_far() {
    let now = 1_790_640_000_i64;
    let dated = |ago: i64| {
        format!(
            "3 failed since {}: why.",
            aterm_types::rfc3339::format_rfc3339(u64::try_from(now - ago).unwrap())
        )
    };
    assert_eq!(update_words::health_entry_delay(&dated(0), now), DAY);
    assert_eq!(
        update_words::health_entry_delay(&dated(3600), now),
        Duration::from_secs(23 * 3600)
    );
    assert_eq!(
        update_words::health_entry_delay(&dated(30 * 3600), now),
        Duration::ZERO
    );
    assert_eq!(update_words::health_entry_delay("no date here", now), DAY);
    // A start in the future (a skewed clock) is no more than a day.
    assert_eq!(update_words::health_entry_delay(&dated(-3600), now), DAY);
}

/// A warning an older parent carried across an update crosses as its excerpt
/// alone (`since Sep 28`), dated to the END of that day — never early — in the
/// latest year whose day has begun. `now` is 2026-09-29T00:00:00Z.
#[test]
fn a_carried_excerpt_dates_the_day_from_the_end_of_its_day() {
    let now = 1_790_640_000_i64;
    let delay = |line: &str, now: i64| update_words::health_entry_delay(line, now);
    assert_eq!(delay("since Sep 29", now), DAY, "today: the whole day");
    assert_eq!(
        delay("since Sep 28", now),
        DAY - Duration::from_secs(1),
        "yesterday, from its last second"
    );
    assert_eq!(delay("since Sep 14", now), Duration::ZERO);
    // 2027-01-02T00:00:00Z: Dec 31 is last year's, not this one's.
    let new_year = now + 95 * 86_400;
    assert_eq!(delay("since Dec 31", new_year), Duration::ZERO);
    assert_eq!(delay("since Jan 2", new_year), DAY);
    // Anything else counts from now.
    for line in ["since Sept 14", "since Sep", "Sep 14", "since Sep 32"] {
        assert_eq!(delay(line, now), DAY, "{line:?}");
    }
}

/// THE OS BANNER HAS ONE DOOR AND ONE CAUSE. With `desktop_alerts` on, a
/// failing lane that nobody here can mend that minute (a stalled check, a
/// sibling's lock, a download that will not verify, an install the lane will
/// try again) posts no banner; the copy in Applications that is not the signed
/// release — `NeedsPerson` — does. The setting still decides last.
#[test]
fn the_banner_is_owed_to_a_cause_only_the_owner_can_end() {
    use crate::messages_host::UPDATE_HEALTH_BANNERS;
    let take = || UPDATE_HEALTH_BANNERS.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let mut app = App::headless_for_test();
    app.config.desktop_alerts = Some(true);
    let _ = take();
    for kind in HealthKind::ALL {
        assert!(
            !app.post_update_health_banner(
                kind,
                "Update check stopped",
                "since 2026-09-14T00:00:00Z:"
            ),
            "{kind:?}"
        );
    }
    assert!(take().is_empty(), "no cause the owner alone can end");
}

/// THE SUPERVISOR THAT KEEPS FAILING is a record when it begins (once per
/// episode, however often it fails again) and a line of the digest once an hour
/// of failures has gone (`harness.supervisor.<tab>.failing`); steady again — a
/// run that outlived its pause without failing, or its worker ended — takes the
/// line away, and before the hour nothing was ever shown. It marks no tab and
/// re-grids nothing. NEGATIVE CONTROLS: a supervisor that failed and then
/// held for its pause and half an hour is over, and is never a line.
#[test]
fn a_supervisor_that_keeps_failing_is_a_record_then_a_line_after_an_hour() {
    let mut app = App::headless_for_test();
    let (message, presence) = (message_regrids(), presence_regrids());
    let pause = Duration::from_secs(300);
    let failing = |why: &str| HostNote::SupervisorFailing {
        sid: "s-7".to_string(),
        why: why.to_string(),
        pause,
    };
    let records = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.title.starts_with("Supervisor keeps stopping"))
            .count()
    };
    app.note_harness_host(failing("boom"));
    app.note_harness_host(failing("boom again"));
    assert_eq!(records(&app), 1, "one record per episode");
    assert!(app.messages.digest().is_empty(), "not before the hour");
    let t0 = app.supervisor_failing["s-7"].first;
    // It keeps failing: the latest failure is always within its pause and the
    // half hour of the next. Fifty-five minutes on, still nothing.
    app.supervisor_failing.get_mut("s-7").unwrap().last = t0 + Duration::from_secs(55 * 60);
    let _ = app.settle_messages(t0 + Duration::from_secs(59 * 60));
    assert!(app.messages.digest().is_empty(), "a minute early");
    app.supervisor_failing.get_mut("s-7").unwrap().last = t0 + Duration::from_secs(59 * 60);
    let _ = app.settle_messages(t0 + Duration::from_secs(60 * 60));
    let entry = &app.messages.digest().entries[0];
    assert_eq!(entry.key, "harness.supervisor.s-7.failing");
    assert!(
        entry.detail.iter().any(|l| l.contains("boom again")),
        "{:?}",
        entry.detail
    );
    assert!(
        app.ripening_deadline().is_some(),
        "and it is watched until steady"
    );
    // Steady again: gone, and the next episode is a new record.
    app.note_harness_host(HostNote::SupervisorSteady {
        sid: "s-7".to_string(),
    });
    assert!(app.messages.digest().is_empty());
    app.note_harness_host(failing("again"));
    assert_eq!(records(&app), 2);
    // NEGATIVE CONTROL: it held for its pause and half an hour with no further
    // failure — over, and never a line.
    let t1 = app.supervisor_failing["s-7"].first;
    let _ = app.settle_messages(t1 + pause + Duration::from_secs(31 * 60));
    assert!(
        app.supervisor_failing.is_empty(),
        "a run that held is steady"
    );
    let _ = app.settle_messages(t1 + 3 * Duration::from_secs(3600));
    assert!(app.messages.digest().is_empty());
    // A worker that ended before the hour: nothing ever shown.
    app.note_harness_host(failing("once more"));
    app.note_harness_host(HostNote::SupervisorSteady {
        sid: "s-7".to_string(),
    });
    assert!(app.ripening_deadline().is_none());
    assert!(app.messages.digest().is_empty());
    assert_eq!((message_regrids(), presence_regrids()), (message, presence));
    assert!(app.messages.live_rows().count() == 0, "no band row");
}

/// A RATING CARD THE HARNESS WILL NOT PRESS: a record now, "press 0 in that tab"
/// an hour on, gone with the card; never a tab mark. A survey the loop is still
/// TRYING to close is a record and nothing more (a persisting "still trying" is
/// the alert the rule forbids).
#[test]
fn a_card_the_harness_will_not_press_is_a_record_then_a_line_after_an_hour() {
    let mut app = App::headless_for_test();
    app.note_harness_host(HostNote::SurveyStuck {
        sid: "s-8".to_string(),
        kind: StuckSurvey::Unclosed,
        why: "the session survey did not close (3 tries); still trying".to_string(),
    });
    assert!(
        app.messages
            .log()
            .records()
            .any(|r| r.title.starts_with("Survey will not close")),
        "a record"
    );
    assert!(
        app.ripening_deadline().is_none(),
        "still trying: no claim on the digest, however long"
    );
    app.note_harness_host(HostNote::SurveyEnded {
        sid: "s-8".to_string(),
    });
    app.note_harness_host(HostNote::SurveyStuck {
        sid: "s-9".to_string(),
        kind: StuckSurvey::Withheld,
        why: "a rating card aterm does not answer is open (odd)".to_string(),
    });
    assert!(app.messages.digest().is_empty(), "not before the hour");
    assert!(
        app.messages
            .log()
            .records()
            .any(|r| r.title == "Rating card open in its tab"),
        "a record"
    );
    let due = app.ripening_deadline().expect("a claim");
    let _ = app.settle_messages(due);
    let entry = &app.messages.digest().entries[0];
    assert_eq!(entry.key, "harness.supervisor.s-9.survey-card");
    assert!(
        entry.then.starts_with("Press 0 in that tab"),
        "{}",
        entry.then
    );
    app.note_harness_host(HostNote::SurveyEnded {
        sid: "s-9".to_string(),
    });
    assert!(app.messages.digest().is_empty(), "gone with the card");
}

/// THE VOLUME ATERM CANNOT HELP: two 6 h passes that freed nothing are one line
/// (`disk.low`), the record says the figures and where the space is (the
/// largest build directories aterm cannot reclaim), and space coming back
/// ends it.
#[test]
fn a_volume_aterm_could_not_help_is_one_line_and_space_ends_it() {
    let mut app = App::headless_for_test();
    let hogs = vec![
        "$HOME/trust/build 94.0 GiB (x.py bootstrap; run scripts/slim_build.py)".to_owned(),
        "$HOME/trust/crates/target.noindex 49.0 GiB (cargo build output: deps/ and build/ are `targo clean`'s)".to_owned(),
        "~/x/target 1.0 GiB (cargo build output: deps/ and build/ are `targo clean`'s)".to_owned(),
    ];
    app.note_harness_host(HostNote::DiskLow {
        free_gib: 7,
        floor_gib: 20,
        hogs: hogs.clone(),
    });
    app.note_harness_host(HostNote::DiskLow {
        free_gib: 6,
        floor_gib: 20,
        hogs: hogs.clone(),
    });
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, ["disk.low"]);
    assert_eq!(
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.title == "Disk space is low")
            .count(),
        1,
        "one record per episode"
    );
    // WHERE THE SPACE IS: the line names the two largest build directories
    // aterm cannot reclaim and what frees each; the record names every one.
    let detail = app.messages.digest().entries[0].detail.clone();
    assert!(
        detail.iter().any(|l| l.contains(&hogs[0])) && detail.iter().any(|l| l.contains(&hogs[1])),
        "{detail:?}"
    );
    let record = app
        .messages
        .log()
        .records()
        .find(|r| r.title == "Disk space is low")
        .expect("the record")
        .detail
        .clone();
    assert!(
        hogs.iter().all(|h| record.iter().any(|l| l.contains(h))),
        "{record:?}"
    );
    app.note_harness_host(HostNote::DiskPlenty);
    assert!(app.messages.digest().is_empty());
    // Plenty with nothing told is nothing.
    app.note_harness_host(HostNote::DiskPlenty);
}

/// THE DEVELOPER TOOLS LIST (owner ruling 2026-10-14) is one line however often a
/// reading says `not_granted`, whose one press opens the Developer Tools pane; a reading
/// that finds aterm on the list ends it; `unknown` leaves it as it was. Never a band row
/// or a re-grid. (The hand-over that opens the pane by itself is the wake handler's, on a
/// windowed instance only: `apply_devtools_step` reports it and opens nothing.)
#[test]
fn the_developer_tools_list_is_one_line_and_a_reading_decides_it() {
    use crate::devtools_watch::Step;
    let mut app = App::headless_for_test();
    let regrids = message_regrids();
    let keys = |app: &App| -> Vec<String> {
        app.messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect()
    };
    assert!(app.apply_devtools_step(Step::Raise { hand_over: true }));
    assert!(!app.apply_devtools_step(Step::Raise { hand_over: false }));
    assert_eq!(
        keys(&app),
        [mr::DEVTOOLS_KEY],
        "one line, however often it is told"
    );
    let line = app.messages.digest().entries[0].clone();
    assert!(
        line.actions.iter().any(|a| matches!(
            a,
            aterm_messages::Intent::OpenSystemPane { pane } if pane == mr::PANE_DEVELOPER_TOOLS
        )),
        "the one press opens Developer Tools: {:?}",
        line.actions
    );
    assert!(!app.apply_devtools_step(Step::Nothing));
    assert_eq!(keys(&app), [mr::DEVTOOLS_KEY], "unknown leaves it");
    assert!(!app.apply_devtools_step(Step::Clear));
    assert!(app.messages.digest().is_empty(), "on the list: it ends");
    assert_eq!(app.message_band_rows, 0, "never a band row");
    assert_eq!(message_regrids(), regrids, "no re-grid");
    // A headless instance never acts on a reading, whatever its slot holds.
    app.on_devtools_probe_ready();
    assert!(app.messages.digest().is_empty());
}

/// A `[disk]` SETTING ATERM CANNOT READ, WHICH TURNED ITS CLEANUP OFF, IS ONE
/// LINE AT ONCE (`disk.settings`, review 30): the look that says it again
/// restates it and never doubles it, it names what was not read, and the look
/// that reads the settings again ends it. A volume short while it holds is a
/// low-disk line of its own form beside it (`disk.low-cleanup-off`): space
/// back ends that line and leaves the setting's, and the settings read again
/// end both, since aterm freeing nothing was that line's premise. NEGATIVE
/// CONTROL: the settings read again leave the passes' `disk.low` standing.
#[test]
fn a_disk_setting_that_turns_cleanup_off_is_one_line_until_it_reads_again() {
    let mut app = App::headless_for_test();
    let keys = |app: &App| {
        let mut keys: Vec<String> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect();
        keys.sort();
        keys
    };
    let records = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.title == "Disk space is low")
            .count()
    };
    let notes =
        vec!["disk.apply must be true or false, not a string, so every removal is off".to_string()];
    let short = HostNote::DiskLowCleanupOff {
        free_gib: 2,
        floor_gib: 10,
    };
    for _ in 0..2 {
        app.note_harness_host(HostNote::DiskSettingsOff {
            notes: notes.clone(),
        });
        app.note_harness_host(short.clone());
    }
    assert_eq!(
        keys(&app),
        [mr::DISK_LOW_CLEANUP_OFF_KEY, mr::DISK_SETTINGS_KEY]
    );
    assert_eq!(records(&app), 1, "one record per episode");
    let settings = app
        .messages
        .digest()
        .entries
        .iter()
        .find(|e| e.key == mr::DISK_SETTINGS_KEY)
        .expect("the settings line")
        .detail
        .clone();
    assert!(
        settings.iter().any(|l| l.contains(&notes[0])),
        "{settings:?}"
    );
    app.note_harness_host(HostNote::DiskPlenty);
    assert_eq!(
        keys(&app),
        [mr::DISK_SETTINGS_KEY],
        "space back is not the setting fixed"
    );
    app.note_harness_host(short.clone());
    assert_eq!(records(&app), 2, "a new episode is recorded again");
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert!(
        app.messages.digest().is_empty(),
        "{:?}: the settings read end the line their fault opened",
        keys(&app)
    );
    // Read again with nothing open is nothing.
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert!(app.messages.digest().is_empty());

    // NEGATIVE CONTROL: the passes' line is the volume's alone.
    app.note_harness_host(HostNote::DiskLow {
        free_gib: 2,
        floor_gib: 10,
        hogs: Vec::new(),
    });
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert_eq!(keys(&app), [mr::DISK_LOW_KEY]);
    app.note_harness_host(HostNote::DiskPlenty);
    assert!(app.messages.digest().is_empty());

    // A cleanup-off line an earlier process raised (adopted: this process
    // holds no mark) ends at the first look that reads the settings.
    let mut app = App::headless_for_test();
    app.except(&mr::disk_low_cleanup_off_entry(2, 10));
    assert!(app.host_noted.is_empty());
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert!(app.messages.digest().is_empty(), "{:?}", keys(&app));
}

/// A SHORT VOLUME IS ONE LOW-DISK LINE, IN THE FORM OF THE RUN THAT TOLD IT
/// (review 30): the cleanup-off form takes the place of the passes' line
/// (the settings broke under a volume already short), and the passes' line
/// takes the place of the cleanup-off form. Each form is recorded once. The
/// passes' line giving way is not its cause ending: what the owner filed of
/// it stays filed until space is back.
#[test]
fn a_short_volume_is_one_low_disk_line_in_the_form_last_told() {
    let mut app = App::headless_for_test();
    let keys = |app: &App| -> Vec<String> {
        app.messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect()
    };
    let passes = HostNote::DiskLow {
        free_gib: 3,
        floor_gib: 10,
        hogs: Vec::new(),
    };
    let off = HostNote::DiskLowCleanupOff {
        free_gib: 2,
        floor_gib: 10,
    };
    app.note_harness_host(passes.clone());
    assert_eq!(keys(&app), [mr::DISK_LOW_KEY]);
    app.note_harness_host(off.clone());
    assert_eq!(keys(&app), [mr::DISK_LOW_CLEANUP_OFF_KEY]);
    app.note_harness_host(off.clone());
    app.note_harness_host(passes.clone());
    assert_eq!(keys(&app), [mr::DISK_LOW_KEY]);
    assert_eq!(
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.title == "Disk space is low")
            .count(),
        2,
        "each form once"
    );

    // A CHANGE OF FORM IS NOT THE CAUSE ENDING: the volume never got space
    // back, so the passes' line the owner filed stays filed through the
    // cleanup-off form, and is quiet when passes tell it again.
    let mut app = App::headless_for_test();
    app.note_harness_host(passes.clone());
    assert_eq!(app.dismiss_exceptions(), 1);
    app.note_harness_host(off);
    assert_eq!(keys(&app), [mr::DISK_LOW_CLEANUP_OFF_KEY]);
    app.note_harness_host(HostNote::DiskSettingsRead);
    assert!(app.messages.digest().is_empty(), "{:?}", keys(&app));
    app.note_harness_host(passes.clone());
    assert!(
        app.messages.digest().is_empty(),
        "{:?}: the owner's filing holds",
        keys(&app)
    );
    // NEGATIVE CONTROL: space back is the cause ending, the filing is
    // forgotten with it, and the next short volume is news.
    app.note_harness_host(HostNote::DiskPlenty);
    app.note_harness_host(passes);
    assert_eq!(keys(&app), [mr::DISK_LOW_KEY]);
}

/// THE UPGRADE THAT CANNOT RUN (its state folder cannot hold the lock) is the
/// host's fact, `harness.upgrade-host.cannot-run`, in the classifier's words, at
/// once; it ends when the LAST tab that could not run runs again (or its worker
/// ends), and a pass over the upgrade's rows never ends it.
#[test]
fn an_upgrade_that_cannot_run_is_one_line_until_the_last_tab_runs_again() {
    let mut app = App::headless_for_test();
    for sid in ["s-1", "s-2"] {
        app.note_harness_host(HostNote::UpgradeCannotRun {
            sid: sid.to_string(),
            step: "busy:state-unwritable".to_string(),
            dir: Some(std::path::PathBuf::from("/state/upgrade")),
        });
    }
    assert_eq!(app.messages.digest().len(), 1, "one line, two tabs");
    let entry = &app.messages.digest().entries[0];
    assert_eq!(entry.key, "harness.upgrade-host.cannot-run");
    assert!(
        entry.then.starts_with("Fix that folder's permissions"),
        "{}",
        entry.then
    );
    // The folder is named under Details; the step word stays in the log.
    let said = format!("{} {} {:?}", entry.title, entry.then, entry.detail);
    assert!(said.contains("/state/upgrade"), "{said}");
    assert!(!said.contains("busy:"), "{said}");
    app.except_replace(Family::Upgrade, &[]);
    assert_eq!(
        app.messages.digest().len(),
        1,
        "the rows' pass does not end it"
    );
    app.note_harness_host(HostNote::UpgradeRuns {
        sid: "s-1".to_string(),
    });
    assert_eq!(app.messages.digest().len(), 1, "s-2 still cannot");
    app.note_harness_host(HostNote::UpgradeRuns {
        sid: "s-2".to_string(),
    });
    assert!(app.messages.digest().is_empty());
}

/// What a note of the accounts may never move: the band's rows, a tab's
/// attention (its strip's bit, its session's badge), and the two re-grid counts.
fn glass(app: &App) -> (usize, usize, usize, u64, u64) {
    let marked = app
        .windows
        .values()
        .flat_map(|w| w.tab_set.tabs())
        .filter(|tab| tab.presentation.indicators.attention)
        .count();
    let badged = app
        .pool
        .iter()
        .filter(|s| {
            let meta = s.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            meta.attention.is_some()
        })
        .count();
    (
        app.messages.live_rows().count(),
        marked,
        badged,
        message_regrids(),
        presence_regrids(),
    )
}

/// THE CLAUDE ACCOUNTS' TWO LINES (`HostNote::Accounts`), walked: an account
/// signed out is ONE line, `harness.accounts.signed-out`, whose key and action
/// hold while its first label does; none signed out ends it. A refused roster
/// is its own line and stands alone: no account can be named until the file is
/// mended. Each end is said at every note, so a line an earlier process left
/// open ends at the first.
///
/// THE LINE'S COMMAND IS ALWAYS THE ONE TO RUN, on every surface: when the
/// account its action is for changes, the open line ends and is raised anew, so
/// its title, its detail and the log's open record (what `aterm exceptions`
/// prints) say the accounts as they are. A count that moves while the first
/// label holds restates in place by the digest's own rule: inside its restate
/// quiet the words wait, and the host saying the line again past it lands them.
///
/// NEGATIVE CONTROLS: the same note again moves nothing, `Nothing` on an empty
/// digest (the machine with no roster) moves nothing, and after every step no
/// band row stands, no tab carries attention, nothing re-grids and the host
/// holds no claim and wrote no record.
#[test]
fn the_accounts_lines_follow_what_the_host_says_and_mark_nothing() {
    use crate::messages_host::wall_stamp_now;
    use aterm_messages::PostOutcome;
    use aterm_messages::need::RESTATE_QUIET;
    use std::time::Instant;
    let mut app = App::headless_for_test();
    let before = glass(&app);
    let records = |app: &App| app.messages.log().records().filter(|r| !r.later).count();
    let recorded = records(&app);
    let listed = |labels: &[&str]| {
        HostNote::Accounts(AccountsSaid::Listed {
            signed_out: labels.iter().map(|l| l.to_string()).collect(),
        })
    };
    let step = |app: &mut App, note: HostNote| -> Vec<(String, String)> {
        app.note_harness_host(note.clone());
        assert_eq!(glass(app), before, "{note:?}: nothing on the glass");
        assert!(app.host_noted.is_empty(), "{note:?}: no claim held");
        assert_eq!(records(app), recorded, "{note:?}: no record");
        let mut lines: Vec<(String, String)> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| (e.key.clone(), e.then.clone()))
            .collect();
        lines.sort();
        lines
    };
    // The signed-out line as the window shows it — its title and its detail
    // past the reason and the action — and as the log holds it open.
    let shown = |app: &App| -> (String, Vec<String>) {
        let digest = app.messages.digest();
        let e = digest
            .entries
            .iter()
            .find(|e| e.key == mr::ACCOUNTS_SIGNED_OUT_KEY)
            .expect("the line is open");
        (e.title.clone(), e.detail[2..].to_vec())
    };
    let logged = |app: &App| -> Vec<String> {
        app.messages
            .log()
            .records()
            .filter(|r| r.later && r.is_live())
            .filter(|r| r.key.as_deref() == Some(mr::ACCOUNTS_SIGNED_OUT_KEY))
            .map(|r| r.detail[1].clone())
            .collect()
    };
    let line = |key: &str, then: &str| (key.to_string(), then.to_string());
    let login = |label: &str| {
        line(
            mr::ACCOUNTS_SIGNED_OUT_KEY,
            &format!("Run aterm harness accounts login {label}"),
        )
    };
    let refused = line(
        mr::ACCOUNTS_REFUSED_KEY,
        "Run aterm harness accounts to read why it is refused",
    );
    let one = "A Claude account is signed out".to_string();

    // The machine with no roster: nothing to end, and nothing moves.
    let rev = app.messages.digest_revision();
    assert!(step(&mut app, HostNote::Accounts(AccountsSaid::Nothing)).is_empty());
    assert!(step(&mut app, listed(&[])).is_empty());
    assert_eq!(app.messages.digest_revision(), rev, "an empty digest stays");

    // One signed out: the line. Said again: nothing moves.
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(
        shown(&app),
        (one.clone(), vec!["Signed out: work-2".to_string()])
    );
    let rev = app.messages.digest_revision();
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(app.messages.digest_revision(), rev, "the same words again");
    let id = app.messages.digest().entries[0].id;
    // A second signs out: the same line, the same action. Its count and its
    // detail wait out the digest's restate quiet...
    let both = ["work-2", "pool-a"];
    assert_eq!(step(&mut app, listed(&both)), [login("work-2")]);
    assert_eq!(app.messages.digest().entries[0].id, id, "one entry, kept");
    assert_eq!(shown(&app).0, one);
    // ...and the host saying the line again past it (its slow look) lands
    // them in place.
    let again = mr::accounts_signed_out_entry(&both.map(str::to_string)).expect("an entry");
    let posted = app.messages.post(
        again.message().expect("an entry that passes"),
        wall_stamp_now(),
        Instant::now() + RESTATE_QUIET,
    );
    assert_eq!(posted.outcome, PostOutcome::Restated);
    assert_eq!(
        shown(&app),
        (
            "2 Claude accounts are signed out".to_string(),
            vec!["Signed out: work-2, pool-a".to_string()]
        )
    );
    assert_eq!(app.messages.digest().entries[0].id, id);
    assert_eq!(logged(&app), ["Run aterm harness accounts login work-2"]);

    // The first signs in: the line is the next account's at once, in the
    // window and in the log — a line of its own, the other one ended.
    assert_eq!(step(&mut app, listed(&["pool-a"])), [login("pool-a")]);
    assert_ne!(app.messages.digest().entries[0].id, id, "raised anew");
    assert_eq!(
        shown(&app),
        (one.clone(), vec!["Signed out: pool-a".to_string()])
    );
    assert_eq!(logged(&app), ["Run aterm harness accounts login pool-a"]);
    // None signed out: gone.
    assert!(step(&mut app, listed(&[])).is_empty());
    assert!(logged(&app).is_empty());

    // A refused roster takes the signed-out line's place, and a roster that
    // admits again gives it back.
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(
        step(&mut app, HostNote::Accounts(AccountsSaid::Refused)),
        std::slice::from_ref(&refused)
    );
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert!(step(&mut app, HostNote::Accounts(AccountsSaid::Nothing)).is_empty());

    // WHAT THE OWNER PUT AWAY STAYS AWAY WHILE THE ROSTER CANNOT BE READ: a
    // refused roster says nothing of who is signed out, so the dismissed line
    // is still quiet once the file is mended...
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(app.dismiss_exceptions(), 1);
    assert_eq!(
        step(&mut app, HostNote::Accounts(AccountsSaid::Refused)),
        [refused]
    );
    assert!(
        step(&mut app, listed(&["work-2"])).is_empty(),
        "still filed"
    );
    // ...and under another account's name too: the fact is its identity.
    assert!(step(&mut app, listed(&["pool-a"])).is_empty());
    // NEGATIVE CONTROL: everyone signed in ends the cause, the filing is
    // forgotten with it, and the next account signed out is news.
    assert!(step(&mut app, listed(&[])).is_empty());
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);

    // LINES AN EARLIER PROCESS LEFT OPEN, adopted from its log with none of
    // its memory. The first note says another account than the adopted line's
    // action (the owner signed `work-2` in while aterm was down): the line is
    // this process's own words at once, not the log's.
    let adopting = || {
        use aterm_messages::{LogLine, MessageCenter, MessageLog};
        let mut earlier = App::headless_for_test();
        earlier.except(&mr::accounts_signed_out_entry(&both.map(str::to_string)).expect("one"));
        earlier.except(&mr::accounts_refused_entry());
        let lines: Vec<LogLine> = earlier
            .messages
            .drain_shelved_for_persist()
            .into_iter()
            .map(|(line, _)| line)
            .collect();
        let mut log = MessageLog::empty();
        log.replay_all(lines);
        let now = Instant::now();
        let mut center = MessageCenter::new(log, now);
        assert_eq!(center.adopt_digest(wall_stamp_now(), now), 2);
        let mut app = App::headless_for_test();
        app.messages = center;
        app
    };
    let mut app = adopting();
    let before = glass(&app);
    assert_eq!(shown(&app).0, "2 Claude accounts are signed out");
    app.note_harness_host(listed(&["pool-a"]));
    assert_eq!(
        shown(&app),
        (one, vec!["Signed out: pool-a".to_string()]),
        "the adopted words gave way"
    );
    assert_eq!(app.messages.digest().len(), 1, "and the refused line ended");
    assert_eq!(logged(&app), ["Run aterm harness accounts login pool-a"]);
    assert_eq!(glass(&app), before);
    // `Nothing` (the roster deleted, or the owner's switch off) ends both.
    let mut app = adopting();
    app.note_harness_host(HostNote::Accounts(AccountsSaid::Nothing));
    assert!(app.messages.digest().is_empty(), "both ended");
    assert_eq!(glass(&app), before);
}

/// THE RESUME LOOP'S EXCEPTIONS REACH THE DIGEST (design-exceptions 9.3, X3d).
/// A `Later` point a tab's loop found only a person can answer is ONE line per
/// kind naming its tabs (`harness.resume.<kind>`), filed once whatever the count;
/// a lost login (`Now`) is the supervisor's own escalation and never a line; a
/// stall no reader names is a line per tab (`harness.resume.<tab>.stalled`); and
/// each ends with its fact, ten minutes after the last look that named it (the
/// digest's clock). NEGATIVE CONTROLS: `Now` raises nothing; a tab whose worker
/// ended takes its lines away with it.
#[test]
fn a_resume_exception_is_one_line_per_kind_and_a_stall_one_per_tab_and_each_ends_with_its_fact() {
    let mut app = App::headless_for_test();
    let told = |app: &mut App, sid: &str, standing: Option<Exception>| {
        app.note_harness_host(HostNote::ResumeException {
            sid: sid.to_string(),
            standing,
        });
    };
    let keys = |app: &App| -> Vec<String> {
        let mut keys: Vec<String> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect();
        keys.sort();
        keys
    };
    // A lost login: the supervisor's, never a line.
    told(
        &mut app,
        "s-1",
        Some(resume_exception(ExceptionKind::Login)),
    );
    assert!(app.messages.digest().is_empty());
    // Two tabs at a spend limit: ONE line; a third tab's cleared goal: another.
    told(
        &mut app,
        "s-1",
        Some(resume_exception(ExceptionKind::Spend)),
    );
    told(
        &mut app,
        "s-2",
        Some(resume_exception(ExceptionKind::Spend)),
    );
    told(
        &mut app,
        "s-3",
        Some(resume_exception(ExceptionKind::GoalCleared)),
    );
    assert_eq!(
        keys(&app),
        ["harness.resume.goal-cleared", "harness.resume.spend"]
    );
    let spend = app
        .messages
        .digest()
        .entries
        .iter()
        .find(|e| e.key == "harness.resume.spend")
        .cloned()
        .expect("spend");
    assert!(
        spend.then.starts_with("Add usage credits"),
        "{}",
        spend.then
    );
    // A stall is its tab's own.
    told(&mut app, "s-2", Some(stall_exception()));
    assert_eq!(
        keys(&app),
        [
            "harness.resume.goal-cleared",
            "harness.resume.s-2.stalled",
            "harness.resume.spend"
        ],
        "the stall replaced tab 2's spend; tab 1 still has it"
    );
    // A spend limit that ends in one tab keeps the line for the other; the last
    // end takes it away after the digest's ten minutes, never at the first look.
    told(&mut app, "s-1", None);
    assert!(keys(&app).contains(&"harness.resume.spend".to_string()));
    told(&mut app, "s-3", None);
    told(&mut app, "s-2", None);
    assert_eq!(app.messages.digest().len(), 3, "one look is a flicker");
    let due = app.messages_deadline().expect("the clock is armed");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    assert!(
        app.messages.digest().is_empty(),
        "no fact stands: {:?}",
        keys(&app)
    );
    // A worker that ends takes its exception with it.
    told(
        &mut app,
        "s-4",
        Some(resume_exception(ExceptionKind::Futile)),
    );
    assert_eq!(app.messages.digest().len(), 1);
    told(&mut app, "s-4", None);
    let due = app.messages_deadline().expect("armed");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    assert!(app.messages.digest().is_empty());
}

/// A TAB'S RELAUNCH, BY THE OWNER'S RULE (2026-09-28: an alert persists only
/// where the owner can act, and says why and what). A relaunch that keeps
/// failing is a record and, for a cause a person clears, a line only after an
/// hour; the owner's own `[harness] relaunch = false` is a record; an agent that
/// cannot be brought back is the tab's mark when a command is known (a record
/// here) and one line of the digest when not; and an agent running again ends
/// every line. NEGATIVE CONTROL: a failure the retry gets past alone is never a
/// line, whatever its age.
#[test]
fn a_relaunch_is_a_record_and_only_what_a_person_alone_can_mend_is_a_line() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let say = |app: &mut App, said| {
        app.note_harness_host(HostNote::Relaunch {
            sid: tab.clone(),
            said,
        });
    };
    let records = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.title.starts_with("Agent is not back"))
            .count()
    };
    // Still trying, nothing a person clears: a record, no line, ever.
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: None,
            restarted: false,
        },
    );
    assert_eq!(records(&app), 1);
    assert!(app.ripening_deadline().is_none(), "nothing ripens");
    let _ = app.settle_messages(Instant::now() + 2 * DAY);
    assert!(app.messages.digest().is_empty());
    // A hold the owner can release: a line, but only once it has lasted an hour.
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: Some("held"),
            restarted: false,
        },
    );
    assert!(app.messages.digest().is_empty(), "not yet");
    let due = app.ripening_deadline().expect("armed for the hour");
    let _ = app.settle_messages(due);
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[0].clone()]);
    // The agent runs again: gone.
    say(&mut app, RelaunchSay::Ended);
    assert!(app.messages.digest().is_empty());
    // The owner's own switch: a record.
    say(
        &mut app,
        RelaunchSay::Limited {
            why: "[harness] relaunch = false".to_string(),
        },
    );
    assert!(app.messages.digest().is_empty());
    // Cannot, with the command for the tab's mark: a record, no line.
    let cannot = |command: Option<&str>| RelaunchSay::Cannot {
        reason: "its shell is gone".to_string(),
        restarted: false,
        command: command.map(str::to_string),
    };
    say(&mut app, cannot(Some("claude --resume 0badf00d")));
    assert!(app.messages.digest().is_empty(), "the tab carries the ask");
    // Cannot, with no command to name: one line of the digest.
    say(&mut app, cannot(None));
    let gone = app.messages.digest().entries[0].clone();
    assert_eq!(gone.key, mr::relaunch_keys(&tab)[1]);
    assert!(gone.then.starts_with("Start the agent"), "{}", gone.then);
    say(&mut app, RelaunchSay::Ended);
    assert!(app.messages.digest().is_empty());
}

/// The sid of the front tab of a headless App, which the store now knows the
/// front session by. A REAL-LENGTH one (22 characters, as `aterm ctl ls` prints
/// them): its digest keys are shortened to the key cap, which is the shape the
/// host's closed-tab and adoption decisions must work on.
fn front_tab_sid(app: &mut App) -> String {
    let local = app
        .focused_session_id(crate::WindowId(0))
        .expect("the front session");
    let mut handle = crate::session_store::test_handle(local);
    handle.sid = aterm_session::SessionId::new(REAL_SID);
    app.store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    REAL_SID.to_string()
}

/// A session id of the length real ones have.
const REAL_SID: &str = "s-3e79b5bde172ad0088ad";
/// The sid of a tab that is not there.
const CLOSED_SID: &str = "s-9f8e7d6c5b4a39281706";

/// A RELAUNCH LINE ENDS WITH ITS TAB (review of 2026-09-29): the worker of a
/// closed tab has no later word, so a claim that is still ripening, and a line
/// already standing, end when no tab shows the session. NEGATIVE CONTROL: the
/// same words for a tab that is there ripen and stand.
#[test]
fn a_relaunch_claim_and_line_end_when_their_tab_is_gone() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let blocked = |sid: &str| HostNote::Relaunch {
        sid: sid.to_string(),
        said: RelaunchSay::Failing {
            blocked_by: Some("held"),
            restarted: false,
        },
    };
    app.note_harness_host(blocked(CLOSED_SID));
    app.note_harness_host(blocked(&tab));
    let due = app.ripening_deadline().expect("two claims");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[0].clone()]);
    // A line raised for a tab that has since closed ends at the next look.
    app.note_harness_host(HostNote::Relaunch {
        sid: CLOSED_SID.to_string(),
        said: RelaunchSay::Cannot {
            reason: "its shell is gone".to_string(),
            restarted: false,
            command: None,
        },
    });
    let _ = app.settle_messages(due + Duration::from_secs(2));
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[0].clone()], "{keys:?}");
}

/// A RELAUNCH LINE AN EARLIER PROCESS LEFT is ended by the adoption sweep unless
/// this process's own worker told of it; one this process told of stands. Short
/// ids and real-length ones (whose keys carry a shortened subject).
#[test]
fn the_adoption_sweep_ends_relaunch_lines_this_process_did_not_tell() {
    for (old, told) in [("s-old", "s-told"), ("s-1a2b3c4d5e6f708192a3", REAL_SID)] {
        let mut app = App::headless_for_test();
        let spot = &places()[0];
        for sid in [old, told] {
            app.except(&mr::relaunch_gone_entry(
                sid,
                spot,
                "its shell is gone",
                false,
            ));
            app.except(&mr::relaunch_blocked_entry(sid, spot, "held", false));
        }
        app.host_noted.insert(format!("relaunch:{told}"));
        app.sweep_adopted_supervisor_lines();
        let mut keys: Vec<String> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect();
        keys.sort();
        let mut want = mr::relaunch_keys(told).to_vec();
        want.sort();
        assert_eq!(keys, want, "{told}");
    }
}

/// A LINE AN EARLIER PROCESS LEFT OPEN for a resume exception no loop of this
/// one tells of ends with the first whole answer after the settle: the family
/// is a level family, and a level family that is never asked would keep it for
/// the 14-day expiry. NEGATIVE CONTROL: before the settle nothing is asked, and a
/// line a loop of this process does tell of stands.
#[test]
fn a_resume_line_an_earlier_process_left_open_ends_after_the_settle() {
    let mut app = App::headless_for_test();
    let stale = mr::resume_exceptions(&[(
        "s-9".to_string(),
        resume_exception(ExceptionKind::Spend),
        places()[0].clone(),
    )]);
    app.except_replace(Family::Resume, &stale);
    assert_eq!(app.messages.digest().len(), 1, "adopted from the log");
    // Not yet settled: nothing is asked.
    app.ripen_exceptions(Instant::now());
    assert!(!app.resume_swept);
    assert_eq!(app.messages.digest().len(), 1);
    // Settled: the whole answer is empty, and the digest's clock ends the line.
    app.ripen_exceptions(Instant::now() + DAY);
    assert!(app.resume_swept);
    let due = app.messages_deadline().expect("the clock is armed");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    assert!(app.messages.digest().is_empty());
    // A loop of this process that tells of one keeps it through the sweep.
    let mut app = App::headless_for_test();
    app.note_harness_host(HostNote::ResumeException {
        sid: "s-9".to_string(),
        standing: Some(resume_exception(ExceptionKind::Spend)),
    });
    app.ripen_exceptions(Instant::now() + DAY);
    let _ = app.settle_messages(Instant::now() + DAY + DAY);
    assert_eq!(app.messages.digest().len(), 1, "still told of");
}

/// The pure half: the same kind in two tabs is ONE entry whose title counts the
/// tabs and whose detail names them, one tab keeps its own place and its `Show
/// tab` button, and a lost login (`Now`) is no entry at all.
#[test]
fn a_kind_in_two_tabs_is_one_entry_that_names_them() {
    let spot = |n: u16| mr::UpgradeSpot {
        place: format!("in tab {n}"),
        nav: Some(aterm_messages::Intent::ShowTab { tab: n, window: 1 }),
    };
    let fact = |sid: &str, n: u16, kind| (sid.to_string(), resume_exception(kind), spot(n));
    let two = mr::resume_exceptions(&[
        fact("s-1", 1, ExceptionKind::Spend),
        fact("s-3", 3, ExceptionKind::Spend),
        fact("s-2", 2, ExceptionKind::Login),
    ]);
    assert_eq!(two.len(), 1, "the login is not an entry");
    let msg = two[0].message().expect("passes the door");
    assert_eq!(msg.title, "Account limit stops Claude in 2 tabs");
    assert!(
        msg.detail.iter().any(|l| l == "In tab 1, tab 3"),
        "{:?}",
        msg.detail
    );
    let one = mr::resume_exceptions(&[fact("s-1", 1, ExceptionKind::GoalCleared)]);
    let msg = one[0].message().expect("passes the door");
    assert_eq!(msg.title, "Claude lost its goal in tab 1");
    assert!(
        msg.actions
            .iter()
            .any(|a| matches!(a, aterm_messages::Intent::ShowTab { tab: 1, .. })),
        "{:?}",
        msg.actions
    );
    let stalled = mr::resume_exceptions(&[fact("s-2", 2, ExceptionKind::Stalled)]);
    assert_eq!(stalled[0].key(), "harness.resume.s-2.stalled");
    assert_eq!(
        stalled[0].message().expect("door").title,
        "Claude in tab 2 has stalled"
    );
    // A dialog no reader names is its own kind, so its key is its own: a
    // dismissal of a tab's stall does not silence a later dialog in it.
    let dialog = mr::resume_exceptions(&[fact("s-2", 2, ExceptionKind::Dialog)]);
    assert_eq!(dialog[0].key(), "harness.resume.s-2.dialog");
    assert_eq!(
        dialog[0].message().expect("door").title,
        "Dialog in tab 2 stops Claude"
    );
    // A context no `/compact` lowers is each tab's own, since the way back is
    // done IN the tab: two tabs, two entries, and a tab at the usage limit
    // beside them keeps the kind-wide line of its own.
    let both = mr::resume_exceptions(&[
        fact("s-1", 1, ExceptionKind::Context),
        fact("s-3", 3, ExceptionKind::Context),
        fact("s-2", 2, ExceptionKind::Futile),
    ]);
    let mut keys: Vec<String> = both.iter().map(|e| e.key()).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "harness.resume.futile",
            "harness.resume.s-1.context",
            "harness.resume.s-3.context"
        ]
    );
    let entry = both
        .iter()
        .find(|e| e.key() == "harness.resume.s-3.context")
        .expect("its own entry");
    assert_eq!(entry.log_line_text(), Some("resume context sid=s-3"));
    let msg = entry.message().expect("passes the door");
    assert_eq!(msg.severity, aterm_messages::Severity::Info);
    assert_eq!(msg.title, "Context in tab 3 cannot be compacted");
    assert!(
        msg.actions
            .iter()
            .any(|a| matches!(a, aterm_messages::Intent::ShowTab { tab: 3, .. })),
        "{:?}",
        msg.actions
    );
}

/// THE STALL CANARY'S 90-MINUTE RUNG is a record and one `aterm.log` error, once
/// per episode, and never a line of the digest and never a mark on a tab: the
/// digest waits for the third hour ([`ExceptionKind::Stalled`]). Its end lets the
/// next episode be told again. NEGATIVE CONTROL: the same episode told twice
/// records once.
#[test]
fn a_stall_at_ninety_minutes_is_a_record_once_and_no_line() {
    let mut app = App::headless_for_test();
    let told = |app: &mut App, minutes| {
        app.note_harness_host(HostNote::ResumeUnread {
            sid: "s-1".to_string(),
            minutes,
        });
    };
    told(&mut app, Some(90));
    told(&mut app, Some(91));
    assert!(app.messages.digest().is_empty(), "no line at 90 minutes");
    assert!(app.host_noted.contains("stalled:s-1"));
    let recorded = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|r| !r.later && r.title.starts_with("Claude has stalled"))
            .count()
    };
    assert_eq!(recorded(&app), 1, "one record per episode");
    told(&mut app, None);
    assert!(!app.host_noted.contains("stalled:s-1"));
    told(&mut app, Some(95));
    assert_eq!(recorded(&app), 2, "a new episode is told again");
}

/// A REPAINT THAT GAVE UP is one line, `render.<tab>.unrepaired`, "Tab N may be
/// one row off / drag the window edge one row and back" — no tab mark, no band
/// row, no re-grid — and only while its epoch is the tab's open, at-risk one: a
/// give-up for an epoch that is not (it healed before the give-up got here)
/// raises nothing, where it once raised a line the next turn ended. A session
/// the store does not know raises nothing.
#[test]
fn a_repaint_that_gave_up_is_one_line_and_none_for_a_healed_epoch() {
    let mut app = App::headless_for_test();
    let (message, presence) = (message_regrids(), presence_regrids());
    // No such session: nothing.
    app.note_render_unrepaired(404, 7, GiveUp::NoAnswer);
    assert!(app.messages.digest().is_empty());
    // A registered one: the line, then its end.
    let Some(session) = app.pool.iter().map(|s| s.id).next() else {
        // A headless test App has no session; the entry itself is checked by the
        // roster and the direct raise below.
        let entry = mr::render_unrepaired_entry("s-5", &places()[0], GiveUp::NoAnswer);
        app.except(&entry);
        assert_eq!(
            app.messages.digest().entries[0].title,
            "Tab 3 may be one row off"
        );
        assert_eq!(
            app.messages.digest().entries[0].key,
            "render.s-5.unrepaired"
        );
        app.except_clear(&mr::render_unrepaired_key("s-5"));
        assert!(app.messages.digest().is_empty());
        assert_eq!((message_regrids(), presence_regrids()), (message, presence));
        return;
    };
    let rev = app.messages.digest_revision();
    app.note_render_unrepaired(session, 7, GiveUp::NoAnswer);
    assert!(
        app.messages.digest().is_empty(),
        "epoch 7 is not at risk: no line"
    );
    assert_eq!(app.messages.digest_revision(), rev, "nothing moved");
    assert_eq!((message_regrids(), presence_regrids()), (message, presence));
}

/// A pool session the store knows (so `render.<sid>.unrepaired` can name it),
/// with its timeline holding an OPEN, at-risk render epoch (the engine owes a
/// row: the synthetic epoch of a restored engine), and the sid it goes by.
fn session_with_open_epoch(app: &mut App, local: u64) -> (u64, String) {
    use crate::resize_ledger::EngineProbe;
    let session = crate::stub_session(local);
    let probe = EngineProbe {
        lifetime: 1,
        debt_shift: 1,
        geom: (24, 80),
        ..EngineProbe::default()
    };
    session
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .evaluate_resizes(probe, 1_000);
    app.pool.insert(session);
    let handle = crate::session_store::test_handle(local);
    let sid = handle.sid.as_str().to_string();
    app.store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    let open = app.pool.get(local).is_some_and(|s| {
        s.ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .resize_epoch_at_risk(1)
    });
    assert!(open, "the fixture's epoch 1 is open and at risk");
    (local, sid)
}

/// THE LINE STANDS WHILE THE EPOCH IS AT RISK, and ends when it heals: a bug
/// that made the line flash and vanish at the next nudge step would end the
/// first half; one that never ended it, the second.
#[test]
fn a_render_line_stands_while_its_epoch_is_at_risk_and_ends_when_it_heals() {
    let mut app = App::headless_for_test();
    let (session, sid) = session_with_open_epoch(&mut app, 41);
    app.note_render_unrepaired(session, 1, GiveUp::NoAnswer);
    assert_eq!(app.messages.digest().len(), 1);
    for _ in 0..3 {
        app.check_render_repairs();
    }
    assert_eq!(
        app.messages.digest().len(),
        1,
        "still at risk: the line stands through every nudge step"
    );
    assert_eq!(
        app.messages.digest().entries[0].key,
        mr::render_unrepaired_key(&sid)
    );
    // The engine owes nothing any more: a real resize or a repaint healed it.
    app.pool
        .get(session)
        .expect("the session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .evaluate_resizes(
            crate::resize_ledger::EngineProbe {
                lifetime: 1,
                full_clears: 1,
                geom: (24, 80),
                ..crate::resize_ledger::EngineProbe::default()
            },
            2_000,
        );
    app.check_render_repairs();
    assert!(app.messages.digest().is_empty(), "healed: the line ends");
}

/// AN ADOPTION NUDGE (a blank agent tab) names no epoch of the ledger, so it is
/// a log line and never a digest line that would flash for one loop turn.
#[test]
fn an_adoption_nudge_that_gave_up_raises_no_digest_line() {
    let mut app = App::headless_for_test();
    let (session, _) = session_with_open_epoch(&mut app, 42);
    let rev = app.messages.digest_revision();
    app.note_render_unrepaired(session, crate::app_nudge::ADOPTION_EPOCH, GiveUp::NoAnswer);
    app.check_render_repairs();
    assert!(app.messages.digest().is_empty());
    assert_eq!(app.messages.digest_revision(), rev, "nothing moved");
}

/// Each give-up is worded by its own cause, which the render stream tells
/// apart: no "busy" for a selection or a driver, no guess between causes, and
/// never the title's "one row off" again.
#[test]
fn a_render_line_words_each_cause_by_itself() {
    let spot = &places()[0];
    let said = |why| {
        let entry = mr::render_unrepaired_entry("s-5", spot, why);
        format!("{:?}", entry.message().expect("passes the rule").detail)
    };
    let causes = [
        (GiveUp::NoAnswer, "did not redraw when aterm asked"),
        (GiveUp::Deferred(Gate::Selection), "text stayed selected"),
        (GiveUp::Deferred(Gate::Lease), "another program was driving"),
        (GiveUp::Deferred(Gate::Busy), "kept drawing"),
        (GiveUp::Budget, "three times this hour"),
    ];
    for (why, want) in causes {
        let text = said(why);
        assert!(text.contains(want), "{why:?}: {text}");
        assert!(!text.contains("row off"), "{why:?}: {text}");
        assert!(!text.contains(" or "), "{why:?}: {text}");
        for (other, theirs) in causes {
            if other != why {
                assert!(!text.contains(theirs), "{why:?} says {other:?}: {text}");
            }
        }
    }
}

/// EVERY EVENT FAMILY'S END ACTS ON AN ADOPTED LINE. The log keeps an event
/// entry open across a restart and an update, and this window adopts it with
/// none of the in-memory facts (`supervisor_failing`, `host_noted`,
/// `render_unrepaired`) that the process which raised it held; the cause ending
/// after the restart must still end the line, or it stands until dismissed.
#[test]
fn the_ends_of_the_event_families_end_lines_an_earlier_process_raised() {
    let mut app = App::headless_for_test();
    let spot = &places()[0];
    let (session, sid) = session_with_open_epoch(&mut app, 43);
    // The adopted state: the entries are open; every process-local map is empty.
    app.except(&mr::disk_low_entry(7, 20, &[]));
    app.except(&mr::disk_settings_entry(&[
        "disk.apply is not read".to_string()
    ]));
    app.except(&mr::upgrade_cannot_run_entry("busy:state-unwritable", None));
    app.except(&mr::supervisor_failing_entry(&sid, spot, "boom"));
    app.except(&mr::survey_entry(&sid, spot, "a rating card is open"));
    app.except(&mr::render_unrepaired_entry(&sid, spot, GiveUp::NoAnswer));
    assert_eq!(app.messages.digest().len(), 6);
    assert!(app.host_noted.is_empty() && app.supervisor_failing.is_empty());
    assert!(app.render_unrepaired.is_empty());
    // The session's epoch is still at risk, but this process holds no claim on it.
    let _ = session;
    for note in [
        HostNote::DiskPlenty,
        HostNote::DiskSettingsRead,
        HostNote::UpgradeRuns { sid: sid.clone() },
        HostNote::SupervisorSteady { sid: sid.clone() },
        HostNote::SurveyEnded { sid: sid.clone() },
    ] {
        app.note_harness_host(note);
    }
    app.check_render_repairs();
    let left: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert!(left.is_empty(), "STALE after the end events: {left:?}");
}

/// A render line whose epoch this process holds AT RISK is not an orphan: the
/// adoption sweep of `check_render_repairs` leaves it to its epoch.
#[test]
fn an_adopted_render_line_ends_but_a_held_one_stands() {
    let mut app = App::headless_for_test();
    let (session, sid) = session_with_open_epoch(&mut app, 44);
    let spot = &places()[0];
    app.except(&mr::render_unrepaired_entry(&sid, spot, GiveUp::NoAnswer));
    app.check_render_repairs();
    assert!(
        app.messages.digest().is_empty(),
        "no epoch is held for an adopted line"
    );
    app.note_render_unrepaired(session, 1, GiveUp::NoAnswer);
    app.check_render_repairs();
    assert_eq!(app.messages.digest().len(), 1, "held: stands");
}

/// The supervisor lines an earlier process left, that this one has heard
/// nothing of for its steady time, end; the ones it holds stand. Run for short
/// ids AND real-length ones (22 characters): a real id does not fit a digest
/// key, so the key carries a shortened subject, and the sweep must still tell
/// the line this process holds from the one it does not (review of 2026-09-29:
/// it read the id back out of the key).
#[test]
fn the_adoption_sweep_ends_supervisor_lines_nothing_has_told_of() {
    for (old, held) in [("s-old", "s-held"), ("s-1a2b3c4d5e6f708192a3", REAL_SID)] {
        let mut app = App::headless_for_test();
        let spot = &places()[0];
        app.except(&mr::supervisor_failing_entry(old, spot, "boom"));
        app.except(&mr::supervisor_failing_entry(held, spot, "boom"));
        app.except(&mr::survey_entry(old, spot, "did not close"));
        app.except(&mr::survey_entry(held, spot, "a card"));
        let now = Instant::now();
        app.supervisor_failing.insert(
            held.to_string(),
            crate::exceptions_host::SupervisorFail {
                first: now,
                last: now,
                pause: Duration::from_secs(1),
                why: "boom".to_string(),
                raised: true,
            },
        );
        app.host_noted.insert(format!("survey:{held}:Withheld"));
        app.sweep_adopted_supervisor_lines();
        let mut keys: Vec<String> = app
            .messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect();
        keys.sort();
        let mut want = vec![mr::supervisor_failing_key(held), mr::survey_key(held)];
        want.sort();
        assert_eq!(keys, want, "{held}");
    }
}

/// THE FULL DISK ACCESS CARD, in the order it is used: the owner presses the
/// button (`Answered`), the probe then reads the grant, and the regression after
/// the NEXT update must be news again.
#[test]
fn a_verified_grant_after_the_owners_answer_makes_the_next_regression_news() {
    use aterm_messages::ActionIndex;
    let mut app = App::headless_for_test();
    let words = crate::consent_card::NeedWords {
        cause: crate::consent_card::NeedCause::PromptObserved {
            service: None,
            session: None,
        },
        tab_number: None,
        regressed: true,
    };
    let regressed = mr::file_access_entry(&words);
    assert_eq!(regressed.key(), mr::FDA_ENTRY_KEYS[1]);
    app.except(&regressed);
    let id = app.messages.digest().entries[0].id;
    let _ = app.messages.act_digest(
        id,
        ActionIndex(0),
        crate::messages_host::wall_stamp_now(),
        Instant::now(),
    );
    assert!(app.messages.digest().is_empty(), "answered: closed");
    app.clear_fda_entries();
    app.except(&regressed);
    assert_eq!(
        app.messages.digest().len(),
        1,
        "the second regression is news after a verified grant"
    );
}

/// FOREIGN TOOLS REACH THE OWNER (audit 2026-10-09: a foreign rustup `trust` link
/// and a managed tool shadowed on PATH were prose 30-odd passes printed — `atpkg said:
/// … not aterm's link; left untouched (fix: …)`, `trust: managed 9192 — SHADOWED by
/// ~/.local/bin/trustdoc` — logged at INFO, in neither messages.log nor `aterm
/// exceptions`). atpkg's `foreign:` lines go through the stdout parser into ONE
/// digest entry per look carrying atpkg's fix, re-said by every pass as the same
/// entry, and ended by that look's first report that sees nothing — the other look's
/// entry untouched. NEGATIVE CONTROL: a scope this build cannot read changes nothing.
#[test]
fn foreign_tools_are_one_entry_per_look_with_atpkgs_fix_ended_when_it_sees_none() {
    use atpkg::cli::{FOREIGN_MARKER, FOREIGN_NONE, FOREIGN_PATH, FOREIGN_SEAM};
    let mut app = App::headless_for_test();
    let key = |scope: &str| crate::toolchain_words::foreign_tools_entry(scope, &[]).key();
    assert_eq!(key(FOREIGN_SEAM), "toolchain.seam.foreign");
    assert_eq!(key(FOREIGN_PATH), "toolchain.path.foreign");
    let feed = |app: &mut App, line: &str| match crate::parse_seed_line(line) {
        Some(crate::Wake::PkgForeign(body)) => app.note_foreign_tools(&body),
        other => panic!("{line}: {other:?}"),
    };
    let entry = |app: &App, scope: &str| {
        app.messages
            .digest()
            .entries
            .into_iter()
            .find(|e| e.key == key(scope))
    };
    let seam = format!(
        "atpkg: {FOREIGN_MARKER}{FOREIGN_SEAM} trust /u/.rustup/toolchains/trust \u{2014} {}",
        atpkg::seam::DETACH_FIX
    );
    let path = format!(
        "atpkg: {FOREIGN_MARKER}{FOREIGN_PATH} trust /u/.local/bin/trustdoc \u{2014} type \
         alab-trustdoc for the managed one"
    );
    let none = |scope: &str| format!("atpkg: {FOREIGN_MARKER}{scope} {FOREIGN_NONE}");
    feed(&mut app, &seam);
    let open = entry(&app, FOREIGN_SEAM).expect("the seam's entry");
    assert_eq!(open.title, "Rustup's toolchain link is not aterm's");
    assert!(
        open.detail
            .iter()
            .any(|l| l.contains(atpkg::seam::DETACH_FIX)
                && l.contains("/u/.rustup/toolchains/trust")),
        "atpkg's fix, whole: {:?}",
        open.detail
    );
    feed(&mut app, &path);
    let open = entry(&app, FOREIGN_PATH).expect("the PATH look's entry");
    assert!(
        open.detail.iter().any(|l| l
            == "/u/.local/bin/trustdoc runs before the managed trust: type alab-trustdoc for \
                the managed one"),
        "{:?}",
        open.detail
    );
    assert_eq!(app.messages.digest().len(), 2, "one per look");
    // The next pass says the same: the same entries, nothing new.
    let revision = app.messages.digest_revision();
    feed(&mut app, &seam);
    feed(&mut app, &path);
    assert_eq!(app.messages.digest_revision(), revision, "no news");
    // NEGATIVE CONTROL: a scope this build cannot read changes nothing.
    feed(
        &mut app,
        &format!("atpkg: {FOREIGN_MARKER}elsewhere {FOREIGN_NONE}"),
    );
    assert_eq!(app.messages.digest().len(), 2);
    // The seam fixed: its look's first report that sees nothing ends its entry, and
    // only its own.
    feed(&mut app, &none(FOREIGN_SEAM));
    assert!(!app.owes_exception(&key(FOREIGN_SEAM)), "ended");
    assert!(
        app.owes_exception(&key(FOREIGN_PATH)),
        "the other look's stands"
    );
    feed(&mut app, &none(FOREIGN_PATH));
    assert!(app.messages.digest().is_empty());
    // An item naming no fix is no exception (review of 2026-10-09): the entry's step
    // says to fix each line as it says, and that line would name nothing.
    feed(
        &mut app,
        &format!("atpkg: {FOREIGN_MARKER}{FOREIGN_PATH} gh /u/.local/bin/gh"),
    );
    assert!(app.messages.digest().is_empty(), "no fix, no entry");
}
