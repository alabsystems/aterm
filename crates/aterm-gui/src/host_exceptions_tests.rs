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
    for cause in [
        crate::session_log_journal::Cause::Full,
        crate::session_log_journal::Cause::Io,
        crate::session_log_journal::Cause::Denied,
        crate::session_log_journal::Cause::Unsupported,
        crate::session_log_journal::Cause::Panic,
    ] {
        all.push(mr::session_journal_paused_entry(cause.word()));
        all.push(mr::session_journal_stopped_entry(cause.word()));
    }
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
    for step in [
        atpkg::devtools::Step::Add,
        atpkg::devtools::Step::SwitchOn,
        atpkg::devtools::Step::ListedOn,
    ] {
        all.push(mr::devtools_entry(step));
    }
    all.push(mr::palette_refused_entry());
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
    all.push(mr::usage_tap_silent_entry());
    // The relaunch's, in every place, for every cause and reason it can name.
    for spot in places() {
        for cause in ["held", "shell-prompt", "other-tab"] {
            all.push(mr::relaunch_blocked_entry("s-3e79", &spot, cause, false));
        }
        for reason in ["its shell is gone", "it ran one prompt and ended"] {
            all.push(mr::relaunch_gone_entry("s-3e79", &spot, reason, false));
            all.push(mr::relaunch_gone_entry("s-3e79", &spot, reason, true));
        }
        for restarted in [false, true] {
            for command in [None, Some("HOME=/Users//o claude --resume 0badf00d")] {
                all.push(mr::relaunch_lost_entry(
                    "s-3e79",
                    &spot,
                    "its shell is gone",
                    restarted,
                    command,
                ));
            }
        }
        all.push(mr::relaunch_not_restored_entry("s-3e79", &spot));
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
fn later_kinds() -> [ExceptionKind; 7] {
    [
        ExceptionKind::Spend,
        ExceptionKind::Bucket,
        ExceptionKind::GoalCleared,
        ExceptionKind::Futile,
        ExceptionKind::Context,
        ExceptionKind::Dialog,
        ExceptionKind::Refused,
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
        ExceptionKind::Refused => {
            return Exception::refused(Some("tool_history_mismatch"), "API Error: 400");
        }
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
        // The old lost login is the loop's own escalation.
        escalated: kind == ExceptionKind::Login,
    }
}

/// The stall canary's third rung, as the loop tells it.
fn stall_exception() -> Exception {
    Exception::stalled()
}

/// EVERY ENTRY THE HOST CAN WRITE passes the door as a `Later` ask (an entry the
/// door refuses is never shown). Keys stay within their cap, sit under their
/// family's prefix, and share nothing with a record's key.
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
        let aterm_messages::need::Need::Later(_) = msg.need() else {
            panic!("{}: an entry is a Later ask", entry.key());
        };
        let key = entry.key();
        assert!(key.chars().count() <= 48, "{key}");
        assert!(key.starts_with(&entry.family().prefix()), "{key}");
        assert!(
            !record_keys.contains(&key),
            "{key}: an entry and a record never share a key"
        );
    }
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
    assert!(
        e.detail.iter().any(|l| l.contains("Sep 14")),
        "the streak's own start: {:?}",
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
            .filter(|r| !r.later && r.key.as_deref() == Some("harness.host.supervisor.s-7"))
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
            .any(|r| r.key.as_deref() == Some("harness.host.survey-open.s-8")),
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
            .any(|r| r.key.as_deref() == Some("harness.host.survey-card.s-9")),
        "a record"
    );
    let due = app.ripening_deadline().expect("a claim");
    let _ = app.settle_messages(due);
    let entry = &app.messages.digest().entries[0];
    assert_eq!(entry.key, "harness.supervisor.s-9.survey-card");
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
            .filter(|r| !r.later && r.key.as_deref() == Some("system.disk.low"))
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
        .find(|r| r.key.as_deref() == Some("system.disk.low"))
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

/// THE FULL DISK, ON THE GLASS ONLY WHILE THE OWNER IS NEEDED: what a pass
/// freed is a fact for later (one `disk.freed` line of the digest, nothing
/// standing); a volume the look left at its emergency is the one standing
/// alert (`system.disk.full`), restated in place, and the volume out of its
/// emergency ends it. NEGATIVE CONTROL: the freed fact puts nothing on the
/// glass.
#[test]
fn a_full_disk_is_one_standing_alert_and_what_was_freed_is_a_fact() {
    let mut app = App::headless_for_test();
    app.note_harness_host(HostNote::DiskFreed {
        bytes: 31 << 30,
        went: vec!["31.0 GiB ~/Library/Caches/aterm/cargo-build.noindex/x/.cargo-build".into()],
    });
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, ["disk.freed"]);
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::DISK_FULL_KEY)
            .is_none(),
        "a fact is not on the glass"
    );
    let full = || HostNote::DiskFull {
        free: 1 << 20,
        volume: std::path::PathBuf::from("/Users//x"),
        freed: 31 << 30,
        hogs: vec!["$HOME/trust/build 94.0 GiB (x.py bootstrap)".into()],
        why: "nothing more is safe".into(),
    };
    app.note_harness_host(full());
    app.note_harness_host(full());
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::DISK_FULL_KEY)
            .is_some(),
        "the alert stands"
    );
    app.note_harness_host(HostNote::DiskFullOver);
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::DISK_FULL_KEY)
            .is_none(),
        "space back ends it"
    );
}

/// THE DEVELOPER TOOLS LIST (owner ruling 2026-10-14) is one line however often a
/// reading says `not_granted`, whose one press opens the Developer Tools pane and whose
/// second capsule opens aterm's own Settings ▸ Security, in `atpkg::devtools`' words (owner
/// ask 2026-10-15: one spelling on every surface); a reading that finds aterm on the list
/// ends it; `unknown` leaves it as it was. Never a band row or a re-grid. (The hand-over
/// that opens the pane by itself is the wake handler's, on a windowed instance only:
/// `apply_devtools_step` reports it and opens nothing.)
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
    assert_eq!(
        line.actions,
        [
            aterm_messages::Intent::OpenSystemPane {
                pane: mr::PANE_DEVELOPER_TOOLS.to_string()
            },
            aterm_messages::Intent::OpenSettings {
                route: crate::native_settings::SettingsRoute::Security
                    .path()
                    .to_string()
            },
        ],
        "the one press opens Developer Tools; beside it, aterm's Security page"
    );
    assert_eq!(
        line.actions
            .iter()
            .map(aterm_messages::Intent::label)
            .collect::<Vec<_>>(),
        ["Open Settings", "Security"],
        "two capsules that never read alike"
    );
    assert_eq!(line.then, atpkg::devtools::HELPER_STEP, "the one action");
    let skips = aterm_containment::SpikeEvidence::UNMEASURED.devtool_skips_first_run_check;
    assert!(
        line.detail.iter().any(|l| l == atpkg::devtools::why(skips)),
        "why, in atpkg's words: {:?}",
        line.detail
    );
    assert!(
        line.detail.iter().any(|l| l
            .starts_with(aterm_containment::consent::DEVTOOLS_SETTINGS_WORDS)
            && l.ends_with(&format!("({})", atpkg::devtools::PLUS_ROUTE))),
        "the path, and the + route beside the drag: {:?}",
        line.detail
    );
    // The line follows what the list holds of aterm as the lane last read it, as
    // the panel and the card do: a row switched off asks for its switch, a row on
    // while macOS still checks names the causes; neither speaks of a drag or of +.
    for (list, title, then, causes) in [
        (
            crate::devtools_helper::test_arms::list(
                aterm_containment::consent::DevToolListing::Off,
                aterm_containment::consent::DevToolListLabel::Read,
                Some(2),
            ),
            "aterm is off in Developer Tools",
            atpkg::devtools::STEP_SWITCH_ON,
            false,
        ),
        (
            crate::devtools_helper::test_arms::on(),
            "macOS still checks programs aterm starts",
            atpkg::devtools::STEP_LISTED_ON,
            true,
        ),
    ] {
        let mut app = App::headless_for_test();
        app.devtools_helper.note_reading(
            crate::devtools_helper::test_arms::reading(
                aterm_containment::DevToolGrant::NotGranted,
                list,
            ),
            std::time::Instant::now(),
        );
        let _ = app.apply_devtools_step(Step::Raise { hand_over: false });
        assert_eq!(keys(&app), [mr::DEVTOOLS_KEY], "one line");
        let line = app.messages.digest().entries[0].clone();
        assert_eq!((line.title.as_str(), line.then.as_str()), (title, then));
        assert_eq!(
            line.detail
                .iter()
                .any(|l| l == atpkg::devtools::HELPER_LISTED_ON),
            causes,
            "{:?}",
            line.detail
        );
        assert!(
            line.detail
                .iter()
                .all(|l| !l.contains(atpkg::devtools::PLUS_ROUTE)),
            "no + route for a listed aterm: {:?}",
            line.detail
        );
    }
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
            .filter(|r| !r.later && r.key.as_deref() == Some("system.disk.low"))
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
            .filter(|r| !r.later && r.key.as_deref() == Some("system.disk.low"))
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
            credits: Vec::new(),
            window_full: Vec::new(),
            due: Vec::new(),
            none_soon: false,
            wrong: Vec::new(),
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
    // The signed-out line as the window shows it — whether its title counts
    // two accounts, and the labels its detail names past the reason and the
    // action — and as the log holds it open.
    let shown = |app: &App| -> (bool, Vec<&'static str>) {
        let digest = app.messages.digest();
        let e = digest
            .entries
            .iter()
            .find(|e| e.key == mr::ACCOUNTS_SIGNED_OUT_KEY)
            .expect("the line is open");
        let detail = e.detail[2..].join("\n");
        let labels = ["work-2", "pool-a"]
            .into_iter()
            .filter(|l| detail.contains(l))
            .collect();
        (e.title.starts_with("2 "), labels)
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

    // The machine with no roster: nothing to end, and nothing moves.
    let rev = app.messages.digest_revision();
    assert!(step(&mut app, HostNote::Accounts(AccountsSaid::Nothing)).is_empty());
    assert!(step(&mut app, listed(&[])).is_empty());
    assert_eq!(app.messages.digest_revision(), rev, "an empty digest stays");

    // One signed out: the line. Said again: nothing moves.
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(shown(&app), (false, vec!["work-2"]));
    let rev = app.messages.digest_revision();
    assert_eq!(step(&mut app, listed(&["work-2"])), [login("work-2")]);
    assert_eq!(app.messages.digest_revision(), rev, "the same words again");
    let id = app.messages.digest().entries[0].id;
    // A second signs out: the same line, the same action. Its count and its
    // detail wait out the digest's restate quiet...
    let both = ["work-2", "pool-a"];
    assert_eq!(step(&mut app, listed(&both)), [login("work-2")]);
    assert_eq!(app.messages.digest().entries[0].id, id, "one entry, kept");
    assert!(!shown(&app).0, "the count waits out the quiet");
    // ...and the host saying the line again past it (its slow look) lands
    // them in place.
    let again = mr::accounts_signed_out_entry(&both.map(str::to_string)).expect("an entry");
    let posted = app.messages.post(
        again.message().expect("an entry that passes"),
        wall_stamp_now(),
        Instant::now() + RESTATE_QUIET,
    );
    assert_eq!(posted.outcome, PostOutcome::Restated);
    assert_eq!(shown(&app), (true, vec!["work-2", "pool-a"]));
    assert_eq!(app.messages.digest().entries[0].id, id);
    assert_eq!(logged(&app), ["Run aterm harness accounts login work-2"]);

    // The first signs in: the line is the next account's at once, in the
    // window and in the log — a line of its own, the other one ended.
    assert_eq!(step(&mut app, listed(&["pool-a"])), [login("pool-a")]);
    assert_ne!(app.messages.digest().entries[0].id, id, "raised anew");
    assert_eq!(shown(&app), (false, vec!["pool-a"]));
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
    assert!(shown(&app).0, "the adopted line counts two");
    app.note_harness_host(listed(&["pool-a"]));
    assert_eq!(
        shown(&app),
        (false, vec!["pool-a"]),
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

/// THE ACCOUNT'S USAGE LINES (`HostNote::Accounts`, accounts v2 S5): usage
/// credits carrying an account's sessions are ONE line,
/// `harness.accounts.credits`, raised once — the same note again moves
/// nothing — and ended when nothing names it (credits over); the week nearly
/// used with credits off is ONE line, `harness.accounts.window-full`, whose
/// action is to turn credits on, or to raise their cap where the vendor said
/// they ran out. Neither marks a tab, puts a row on the band or writes a
/// record. NEGATIVE CONTROLS: a note that names neither raises neither (the
/// window-full line needs credits known off: the host names none
/// otherwise), and both Entries pass the door's caps and vocabulary.
#[test]
fn the_accounts_usage_lines_follow_what_the_host_says_and_mark_nothing() {
    let mut app = App::headless_for_test();
    let before = glass(&app);
    let said = |credits: &[&str], full: &[(&str, &str)]| {
        HostNote::Accounts(AccountsSaid::Listed {
            signed_out: Vec::new(),
            credits: credits.iter().map(|l| l.to_string()).collect(),
            window_full: full
                .iter()
                .map(|(l, why)| (l.to_string(), why.to_string()))
                .collect(),
            due: Vec::new(),
            none_soon: false,
            wrong: Vec::new(),
        })
    };
    let step = |app: &mut App, note: HostNote| -> Vec<(String, String)> {
        app.note_harness_host(note.clone());
        assert_eq!(glass(app), before, "{note:?}: nothing on the glass");
        assert!(app.host_noted.is_empty(), "{note:?}: no claim held");
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
    let credits = (
        mr::ACCOUNTS_CREDITS_KEY.to_string(),
        "Review the usage credit cap in claude.ai settings".to_string(),
    );
    let turn_on = (
        mr::ACCOUNTS_WINDOW_FULL_KEY.to_string(),
        "Turn on usage credits in claude.ai settings".to_string(),
    );
    let raise = (
        mr::ACCOUNTS_WINDOW_FULL_KEY.to_string(),
        "Raise the usage credit cap in claude.ai settings".to_string(),
    );
    // NEGATIVE CONTROL: nothing named, nothing raised.
    assert!(step(&mut app, said(&[], &[])).is_empty());
    // Credits in use: the line, once.
    assert_eq!(
        step(&mut app, said(&["main"], &[])),
        std::slice::from_ref(&credits)
    );
    let rev = app.messages.digest_revision();
    assert_eq!(
        step(&mut app, said(&["main"], &[])),
        std::slice::from_ref(&credits)
    );
    assert_eq!(app.messages.digest_revision(), rev, "the same words again");
    let e = app.messages.digest().entries[0].clone();
    assert_eq!(e.title, "Claude is running on usage credits");
    assert!(
        e.detail.iter().any(|l| l == "On credits: main"),
        "{:?}",
        e.detail
    );
    // Over: the line ends.
    assert!(step(&mut app, said(&[], &[])).is_empty());
    // The week nearly used with credits off, and spent.
    assert_eq!(
        step(&mut app, said(&[], &[("main", "org_level_disabled")])),
        std::slice::from_ref(&turn_on)
    );
    assert!(step(&mut app, said(&[], &[])).is_empty());
    assert_eq!(
        step(&mut app, said(&[], &[("main", "out_of_credits")])),
        [raise]
    );
    // The owner's switch off, or no roster: both end.
    assert_eq!(
        step(&mut app, said(&["main"], &[("main", "org_level_disabled")])).len(),
        2
    );
    assert!(step(&mut app, HostNote::Accounts(AccountsSaid::Nothing)).is_empty());
    // The door's own checks, every wording.
    for entry in [
        mr::accounts_credits_entry(&["main".to_string(), "work-2".to_string()]),
        mr::accounts_window_full_entry(&[("main".to_string(), "out_of_credits".to_string())]),
        mr::accounts_window_full_entry(&[("main".to_string(), String::new())]),
    ] {
        let entry = entry.expect("an entry");
        assert_eq!(entry.family(), Family::Accounts);
        assert!(entry.message().is_ok(), "{:?}", entry.message().err());
    }
    assert!(mr::accounts_credits_entry(&[]).is_none());
    assert!(mr::accounts_window_full_entry(&[]).is_none());
}

/// THE RESUME LOOP'S EXCEPTIONS REACH THE DIGEST (design-exceptions 9.3, X3d).
/// A `Later` point a tab's loop found only a person can answer is ONE line per
/// kind naming its tabs (`harness.resume.<kind>`), filed once whatever the count;
/// a lost login the supervisor escalated (`Now`) is that escalation's and never
/// a line; a stall no reader names is a line per tab
/// (`harness.resume.<tab>.stalled`); and each ends with its fact, ten minutes
/// after the last look that named it (the digest's clock). NEGATIVE CONTROLS:
/// `Now` raises no line; a tab whose worker ended takes its lines away with it.
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
            .filter(|r| {
                !r.later && r.key.as_deref() == Some(&*format!("harness.host.relaunch.{tab}"))
            })
            .count()
    };
    // Still trying, nothing a person clears, the tab marked with the command
    // (its host's word): a record, no line, ever.
    let command = Some("claude --resume 0badf00d-1111-2222-3333-444455556666".to_string());
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: None,
            restarted: false,
            command: command.clone(),
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
            command,
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
    let cannot = |command: Option<&str>, marked: bool, lost: bool| RelaunchSay::Cannot {
        reason: "its shell is gone".to_string(),
        restarted: false,
        command: command.map(str::to_string),
        marked,
        lost,
    };
    say(
        &mut app,
        cannot(Some("claude --resume 0badf00d"), true, true),
    );
    assert!(app.messages.digest().is_empty(), "the tab carries the ask");
    // Cannot, with no command to name and no conversation: one line of the
    // digest.
    say(&mut app, cannot(None, false, false));
    let gone = app.messages.digest().entries[0].clone();
    assert_eq!(gone.key, mr::relaunch_keys(&tab)[1]);
    say(&mut app, RelaunchSay::Ended);
    assert!(app.messages.digest().is_empty());
    // THE CONVERSATION KNOWN, NO MARK (a command too long for it): its own
    // line, which an agent running in the tab again does not end — only the
    // conversation held again does (LC-4).
    say(
        &mut app,
        cannot(Some("HOME=/o claude --resume 0badf00d"), false, true),
    );
    let lost = app.messages.digest().entries[0].clone();
    assert_eq!(lost.key, mr::relaunch_keys(&tab)[2]);
    assert!(
        lost.then.starts_with("Run the command below"),
        "{}",
        lost.then
    );
    say(&mut app, RelaunchSay::Ended);
    assert_eq!(
        app.messages.digest().entries.len(),
        1,
        "not ended by a program"
    );
    say(&mut app, RelaunchSay::Back);
    assert!(app.messages.digest().is_empty(), "held again");
    // A mark that steps down: the same line, as a fact for later.
    say(
        &mut app,
        RelaunchSay::SteppedDown {
            reason: "its shell is gone".to_string(),
            restarted: false,
            command: Some("claude --resume 0badf00d".to_string()),
        },
    );
    assert_eq!(
        app.messages.digest().entries[0].key,
        mr::relaunch_keys(&tab)[2]
    );
    say(&mut app, RelaunchSay::Back);
    assert!(app.messages.digest().is_empty());
    // ONE WORD PER TAB, THE NEWEST: an older conversation's line, then a
    // newer conversation's mark in the same tab — the line goes with it
    // (the host tracks the newer word alone, whose `Back` would end the
    // older line, and the older conversation held again ends nothing).
    say(
        &mut app,
        cannot(Some("HOME=/o claude --resume 0badf00d"), false, true),
    );
    assert_eq!(
        app.messages.digest().entries[0].key,
        mr::relaunch_keys(&tab)[2]
    );
    say(
        &mut app,
        cannot(Some("claude --resume 5ca1ab1e"), true, true),
    );
    assert!(
        app.messages.digest().is_empty(),
        "the mark is the tab's one word: {:?}",
        app.messages.digest().entries
    );
    // WHAT A RESTART LOST (XREV-LC-2): one line of its own, which the agent
    // running again does not end — what it lost stays lost.
    say(&mut app, RelaunchSay::NotRestored);
    let entries = app.messages.digest().entries.clone();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].key, mr::relaunch_not_restored_key(&tab));
    assert!(!mr::relaunch_keys(&tab).contains(&entries[0].key));
    assert!(
        entries[0].then.starts_with("Look at the tab"),
        "{}",
        entries[0].then
    );
    say(&mut app, RelaunchSay::Ended);
    assert_eq!(app.messages.digest().entries.len(), 1, "Ended leaves it");
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
/// already standing, end when no tab shows the session — the line of what a
/// restart did not bring back too (`not-restored`, the decision review's
/// RR-LC-2: it has no ending word at all, and stood with a Show-tab link to a
/// tab that was gone). NEGATIVE CONTROL: the same words for a tab that is there
/// ripen and stand, and its `not-restored` line stands through `Ended` and the
/// settle.
#[test]
fn a_relaunch_claim_and_line_end_when_their_tab_is_gone() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let blocked = |sid: &str| HostNote::Relaunch {
        sid: sid.to_string(),
        said: RelaunchSay::Failing {
            blocked_by: Some("held"),
            restarted: false,
            command: Some("claude --resume 0badf00d-1111-2222-3333-444455556666".to_string()),
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
            marked: false,
            lost: false,
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
    // What a restart did not bring back: the closed tab's line ends at the
    // next look, the live tab's stands through `Ended` and the settle.
    for sid in [CLOSED_SID, tab.as_str()] {
        app.note_harness_host(HostNote::Relaunch {
            sid: sid.to_string(),
            said: RelaunchSay::NotRestored,
        });
    }
    app.note_harness_host(HostNote::Relaunch {
        sid: tab.clone(),
        said: RelaunchSay::Ended,
    });
    let _ = app.settle_messages(due + Duration::from_secs(3));
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_not_restored_key(&tab)], "{keys:?}");
}

/// A `lost` LINE ENDS WITH ITS TAB TOO (the review of ATERM-KI-1015-1 LC-4):
/// it says "run the command below in that tab" and shows that tab, and its
/// host forgets the tab's word with it, so nothing would end it once the
/// tab is gone — a Warn that resurfaces for the rest of the process, its
/// action pointing at a tab that is gone or another tab now. A Codex's
/// every `Cannot` with a known thread is such a line (it has no command for
/// a mark). NEGATIVE CONTROL: the same line for a tab that is there stands.
#[test]
fn a_lost_line_ends_when_its_tab_is_gone() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let lost = |sid: &str| HostNote::Relaunch {
        sid: sid.to_string(),
        said: RelaunchSay::Cannot {
            reason: "no try could type at its prompt in time".to_string(),
            restarted: false,
            command: Some("HOME=/Users//o claude --resume 0badf00d".to_string()),
            marked: false,
            lost: true,
        },
    };
    app.note_harness_host(lost(CLOSED_SID));
    app.note_harness_host(lost(&tab));
    let _ = app.settle_messages(Instant::now() + Duration::from_secs(1));
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[2].clone()], "{keys:?}");
}

/// TWO LOSSES OF ONE TAB, EACH ENDED BY ITS OWN END (the merge of LC-4 into
/// XREV-LC-2): what a restart did not bring back (`not-restored`, a monitor
/// the resumed build said is gone) and an agent that cannot come back on a
/// known conversation (`lost`) stand side by side, neither writing over the
/// other's words. `Ended` ends neither; the conversation held again
/// (`Back`) and a newer marked word of the tab end only `lost`; a second
/// loss rewrites only its own line; the tab closing ends both; a seamless
/// successor's adoption sweep ends only `lost` (a word the earlier host
/// held), never the fact. NEGATIVE CONTROL: a live tab's two lines stand
/// through the closed-tab sweep.
#[test]
fn a_lost_word_and_what_a_restart_did_not_bring_back_end_each_on_its_own() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let say = |app: &mut App, sid: &str, said| {
        app.note_harness_host(HostNote::Relaunch {
            sid: sid.to_string(),
            said,
        });
    };
    let cannot = |marked: bool| RelaunchSay::Cannot {
        reason: "no try could type at its prompt in time".to_string(),
        restarted: false,
        command: Some("HOME=/Users//o claude --resume 0badf00d".to_string()),
        marked,
        lost: true,
    };
    let lines = |app: &App| -> Vec<(String, String)> {
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
    let [_, _, lost] = mr::relaunch_keys(&tab);
    let not_restored = mr::relaunch_not_restored_key(&tab);
    assert_ne!(lost, not_restored, "two keys");
    let lost_line = (
        lost.clone(),
        "Run the command below in that tab to resume its conversation".to_string(),
    );
    let not_restored_line = (
        not_restored.clone(),
        "Look at the tab and restore what it relied on".to_string(),
    );
    let both = {
        let mut both = vec![lost_line.clone(), not_restored_line.clone()];
        both.sort();
        both
    };
    say(&mut app, &tab, RelaunchSay::NotRestored);
    say(&mut app, &tab, cannot(false));
    assert_eq!(lines(&app), both, "side by side");
    say(&mut app, &tab, RelaunchSay::NotRestored);
    assert_eq!(lines(&app), both, "a second loss rewrites its own line");
    say(&mut app, &tab, RelaunchSay::Ended);
    assert_eq!(lines(&app), both, "an agent running ends neither");
    say(&mut app, &tab, RelaunchSay::Back);
    assert_eq!(
        lines(&app),
        std::slice::from_ref(&not_restored_line),
        "held again"
    );
    say(&mut app, &tab, cannot(false));
    say(&mut app, &tab, cannot(true));
    assert_eq!(
        lines(&app),
        std::slice::from_ref(&not_restored_line),
        "a newer mark"
    );
    say(&mut app, &tab, cannot(false));
    assert_eq!(lines(&app), both);
    // The tab closing ends both; the live tab's stand (NEGATIVE CONTROL).
    say(&mut app, CLOSED_SID, RelaunchSay::NotRestored);
    say(&mut app, CLOSED_SID, cannot(false));
    assert_eq!(app.messages.digest().entries.len(), 4);
    let _ = app.settle_messages(Instant::now() + Duration::from_secs(1));
    assert_eq!(lines(&app), both, "the closed tab's two lines end");
    // A seamless successor adopts both lines and no word: the word's line
    // ends at its sweep, the fact stands.
    let mut successor = App::headless_for_test();
    let spot = &places()[0];
    successor.except(&mr::relaunch_lost_entry(
        &tab,
        spot,
        "its shell is gone",
        false,
        Some("claude --resume 0badf00d"),
    ));
    successor.except(&mr::relaunch_not_restored_entry(&tab, spot));
    successor.sweep_adopted_supervisor_lines();
    let keys: Vec<String> = successor
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [not_restored]);
}

/// The closed-tab sweep runs on every loop turn and judges only what is
/// there to judge: with no relaunch line or claim open, a told mark alone
/// still ends with its tab, and every other fact the host holds stands.
/// NEGATIVE CONTROL: the open tab's told mark stands too.
#[test]
fn a_told_relaunch_mark_alone_ends_with_its_tab_and_nothing_else_moves() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    app.host_noted.insert("stalled:s-1".to_string());
    app.host_noted.insert(format!("relaunch:{tab}"));
    app.host_noted.insert(format!("relaunch:{CLOSED_SID}"));
    let digest_before = app.messages.digest_revision();
    app.end_relaunch_lines_of_closed_tabs();
    let mut noted: Vec<&str> = app.host_noted.iter().map(String::as_str).collect();
    noted.sort_unstable();
    let open_mark = format!("relaunch:{tab}");
    assert_eq!(noted, [open_mark.as_str(), "stalled:s-1"]);
    assert_eq!(app.messages.digest_revision(), digest_before);
    // Nothing relaunch-shaped left: a turn changes nothing at all.
    app.host_noted.remove(&open_mark);
    app.end_relaunch_lines_of_closed_tabs();
    assert_eq!(
        app.host_noted.iter().collect::<Vec<_>>(),
        [&"stalled:s-1".to_string()]
    );
    assert_eq!(app.messages.digest_revision(), digest_before);
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
            app.except(&mr::relaunch_lost_entry(
                sid,
                spot,
                "its shell is gone",
                false,
                Some("claude --resume 0badf00d-1111-2222-3333-444455556666"),
            ));
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

/// The loop's words for a lost login waited at with no account left to move
/// to (`policy/exception.rs`): `Now`, and no escalation of the loop's carries
/// it.
fn stuck_login_exception() -> Exception {
    Exception {
        kind: ExceptionKind::Login,
        when: Urgency::Now,
        why: "Its login is gone, and the roster has no other account to move it to.".to_string(),
        action: "Run aterm harness accounts login pool-a.".to_string(),
        clears: "the login signed in again, an account to move to, or the agent working",
        escalated: false,
    }
}

/// A NOW NO ESCALATION OF THE LOOP'S CARRIES IS THE OWNER'S ASK ON THE GLASS
/// (the alert rule: the owner is needed now, aterm cannot settle it, and one
/// step changes it): a tab's lost login waited at with no account left is ONE
/// standing row, `harness.resume-ask.<tab>`, `Now`-class, in the loop's words
/// — its why and its one step, `Run aterm harness accounts login <label>.` —
/// and no line of the digest. Told again in the same words it is the same
/// row; its end (`None`: the move onto the login signed in again was asked)
/// takes the row down, as the loop's own escalation taking its place does. A
/// row an earlier process carried here that no loop of this one tells of ends
/// at the resume family's settle, and a told one stands. NEGATIVE CONTROLS:
/// the same login carried by the loop's own escalation (`/login` typed, its
/// badge) raises no row and no line, and a `Later` one is a line and no row.
#[test]
fn a_now_no_escalation_carries_is_a_standing_ask_until_its_point_moves_on() {
    use aterm_messages::need::Need;
    let mut app = App::headless_for_test();
    let told = |app: &mut App, sid: &str, standing: Option<Exception>| {
        app.note_harness_host(HostNote::ResumeException {
            sid: sid.to_string(),
            standing,
        });
    };
    let asks = |app: &App| -> Vec<String> {
        app.messages
            .live_rows()
            .filter(|row| matches!(row.msg.need(), Need::Now(_)))
            .map(|row| row.msg.key.clone().unwrap_or_default())
            .collect()
    };
    // NEGATIVE CONTROLS: the loop's own escalation; a `Later` one.
    told(
        &mut app,
        "s-1",
        Some(resume_exception(ExceptionKind::Login)),
    );
    assert!(asks(&app).is_empty(), "the loop's escalation says it");
    assert!(app.messages.digest().is_empty());
    told(
        &mut app,
        "s-1",
        Some(resume_exception(ExceptionKind::Spend)),
    );
    assert!(asks(&app).is_empty());
    assert_eq!(app.messages.digest().len(), 1, "a line, no row");
    told(&mut app, "s-1", None);
    // The ask.
    let lines = app.messages.digest().len();
    told(&mut app, "s-1", Some(stuck_login_exception()));
    let key = mr::resume_ask_key("s-1");
    assert_eq!(asks(&app), std::slice::from_ref(&key));
    assert_eq!(app.messages.digest().len(), lines, "no line of the digest");
    let row = app.messages.live_by_key(&key).expect("the ask");
    assert!(matches!(row.msg.hold, aterm_messages::Hold::Standing(_)));
    assert!(
        row.msg
            .detail
            .iter()
            .any(|l| l == "Run aterm harness accounts login pool-a."),
        "{:?}",
        row.msg.detail
    );
    let id = row.id;
    told(&mut app, "s-1", Some(stuck_login_exception()));
    assert_eq!(
        app.messages.live_by_key(&key).map(|row| row.id),
        Some(id),
        "the same row"
    );
    // Its end takes it down.
    told(&mut app, "s-1", None);
    assert!(asks(&app).is_empty(), "the move asked");
    // …and so does the loop's own escalation taking its place.
    told(&mut app, "s-2", Some(stuck_login_exception()));
    assert_eq!(asks(&app), [mr::resume_ask_key("s-2")]);
    told(
        &mut app,
        "s-2",
        Some(resume_exception(ExceptionKind::Login)),
    );
    assert!(asks(&app).is_empty(), "said once, by the escalation");
    // A row an earlier process carried, which no loop of this one tells of,
    // ends at the settle; the one a loop tells of stands.
    let mut app = App::headless_for_test();
    app.post_message(mr::resume_ask(
        "s-9",
        &places()[0],
        &stuck_login_exception(),
    ));
    told(&mut app, "s-3", Some(stuck_login_exception()));
    app.ripen_exceptions(Instant::now());
    assert_eq!(asks(&app).len(), 2, "not yet settled");
    app.ripen_exceptions(Instant::now() + DAY);
    assert_eq!(asks(&app), [mr::resume_ask_key("s-3")]);
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
    assert!(msg.title.contains("2 tabs"), "{}", msg.title);
    assert!(
        msg.detail.iter().any(|l| l.contains("tab 1, tab 3")),
        "{:?}",
        msg.detail
    );
    let one = mr::resume_exceptions(&[fact("s-1", 1, ExceptionKind::GoalCleared)]);
    let msg = one[0].message().expect("passes the door");
    assert!(
        msg.actions
            .iter()
            .any(|a| matches!(a, aterm_messages::Intent::ShowTab { tab: 1, .. })),
        "{:?}",
        msg.actions
    );
    let stalled = mr::resume_exceptions(&[fact("s-2", 2, ExceptionKind::Stalled)]);
    assert_eq!(stalled[0].key(), "harness.resume.s-2.stalled");
    // A dialog no reader names is its own kind, so its key is its own: a
    // dismissal of a tab's stall does not silence a later dialog in it.
    let dialog = mr::resume_exceptions(&[fact("s-2", 2, ExceptionKind::Dialog)]);
    assert_eq!(dialog[0].key(), "harness.resume.s-2.dialog");
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
            .filter(|r| !r.later && r.key.as_deref() == Some("harness.host.stalled.s-1"))
            .count()
    };
    assert_eq!(recorded(&app), 1, "one record per episode");
    told(&mut app, None);
    assert!(!app.host_noted.contains("stalled:s-1"));
    told(&mut app, Some(95));
    assert_eq!(recorded(&app), 2, "a new episode is told again");
}

/// A PALETTE SET THE OWNER'S CONFIG REFUSED is one digest line for every tab,
/// naming the setting: never a band row, and a session's later refusals (or
/// another session's first) add nothing.
#[test]
fn a_refused_palette_set_is_one_line_naming_the_setting() {
    let mut app = App::headless_for_test();
    assert!(app.messages.digest().is_empty());
    app.note_palette_refused();
    let rev = app.messages.digest_revision();
    app.note_palette_refused();
    assert_eq!(
        app.messages.digest_revision(),
        rev,
        "a second refusal moves nothing"
    );
    let digest = app.messages.digest();
    assert_eq!(digest.entries.len(), 1);
    assert_eq!(digest.entries[0].key, "terminal.palette-refused");
    let msg = mr::palette_refused_entry()
        .message()
        .expect("the entry is well formed");
    let words = format!("{msg:?}");
    assert!(
        words.contains("allow_palette_reconfigure"),
        "the line names the setting that refused it: {words}"
    );
}

/// TURNING PALETTE SETS BACK ON ends the refusal line: its one action is
/// done. A config that still says `false` keeps it (negative control). Both go
/// through the real admission path a config watcher feeds.
#[test]
fn turning_palette_sets_on_clears_the_refusal_line() {
    fn apply(app: &mut App, dir: &std::path::Path, text: &str) {
        let path = dir.join("aterm.toml");
        std::fs::write(&path, text).expect("write config");
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .expect("observe");
        let config: crate::app_config::Config = aterm_toml::from_str(text).expect("valid toml");
        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                values: std::collections::BTreeMap::new(),
                disk_faults: std::sync::Arc::default(),
                assets: config.resolve_asset_catalog(),
                path_feed_fps: config.path_feed_fingerprints(),
                sparkle: config.prepare_sparkle_runtime(),
                fonts: None,
                warnings: Vec::new(),
                font_refusals: Vec::new(),
                reason: crate::app_colorscheme::ThemeChange::ConfigReload,
                config,
            },
        );
    }
    let refused = |app: &App| {
        app.messages
            .digest()
            .entries
            .iter()
            .any(|e| e.key == mr::palette_refused_key())
    };
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-palette-refused")
        .tempdir()
        .expect("scratch dir");
    let mut app = App::headless_for_test();
    apply(
        &mut app,
        scratch.path(),
        "allow_palette_reconfigure = false\n",
    );
    app.note_palette_refused();
    assert!(refused(&app), "precondition: the line is filed");

    let unrelated = "allow_palette_reconfigure = false\nfaint_opacity = 0.4\n";
    apply(&mut app, scratch.path(), unrelated);
    assert!(refused(&app), "still refused: the line stays");

    apply(
        &mut app,
        scratch.path(),
        "allow_palette_reconfigure = true\n",
    );
    assert!(!refused(&app), "allowed again: the line ends");
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
    // A registered one whose epoch is not at risk: nothing either.
    let session = app.pool.iter().map(|s| s.id).next().expect("session 0");
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
        open.detail
            .iter()
            .any(|l| l.contains("/u/.local/bin/trustdoc")
                && l.contains("type alab-trustdoc for the managed one")),
        "atpkg's fix, whole: {:?}",
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

/// A RELAUNCH THAT KEEPS FAILING WITH NO COMMAND THE TAB'S MARK COULD NAME
/// (a Codex, a launch no resume repeats) is the digest's line AT ONCE (LC-5 of
/// ATERM-KI-1015-1: at attention, where it was a record only): the agent is
/// down and only the owner can start it sooner; an agent running again ends
/// it. NEGATIVE CONTROL: with a command the tab's mark says it, and the digest
/// stays empty.
#[test]
fn a_failing_relaunch_with_no_command_is_the_digests_line_at_once() {
    let mut app = App::headless_for_test();
    let tab = front_tab_sid(&mut app);
    let say = |app: &mut App, said| {
        app.note_harness_host(HostNote::Relaunch {
            sid: tab.clone(),
            said,
        });
    };
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: None,
            restarted: false,
            command: Some("claude --resume 0badf00d-1111-2222-3333-444455556666".to_string()),
        },
    );
    assert!(app.messages.digest().is_empty(), "the tab's mark says it");
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: None,
            restarted: false,
            command: None,
        },
    );
    let keys: Vec<String> = app
        .messages
        .digest()
        .entries
        .iter()
        .map(|e| e.key.clone())
        .collect();
    assert_eq!(keys, [mr::relaunch_keys(&tab)[0].clone()], "at once");
    say(&mut app, RelaunchSay::Ended);
    assert!(app.messages.digest().is_empty(), "an agent back ends it");
    // Its conversation open in another tab: no command to run here, and the
    // line at once says to close it there (the review of the r20 relaunch
    // lane: the mark told the second copy).
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: Some("other-tab"),
            restarted: false,
            command: None,
        },
    );
    let line = app.messages.digest().entries[0].clone();
    assert_eq!(line.key, mr::relaunch_keys(&tab)[0], "at once");
    assert!(
        line.then
            .starts_with("Close the conversation in its other tab"),
        "{}",
        line.then
    );
    say(&mut app, RelaunchSay::Ended);
    // NEGATIVE CONTROL: the harness's own restart's carry, which names no
    // command, is a record: its record's row speaks.
    say(
        &mut app,
        RelaunchSay::Failing {
            blocked_by: None,
            restarted: true,
            command: None,
        },
    );
    assert!(app.messages.digest().is_empty(), "a record, its row speaks");
}

/// A renewable login's stuck tab, as the loop tells it (accounts v2 S7): the
/// policy's words, its step `Run aterm harness accounts renew <label>.`.
fn ended_login_exception(label: &str) -> Exception {
    Exception {
        kind: ExceptionKind::Login,
        when: Urgency::Now,
        why: "Its login has ended, and only a renewal in your browser restores it.".to_string(),
        action: format!(
            "{}{label}.",
            aterm_agent::supervise::policy::exception::RENEW_STEP
        ),
        clears: "the login signed in again, an account to move to, or the agent working",
        escalated: false,
    }
}

/// EVERY TAB STOPPED ON AN ENDED, RENEWABLE LOGIN IS IN ONE ASK PER MACHINE
/// (`harness.accounts.renew-ask`): restated in place as tabs join it — its
/// step renewing every label — and withdrawn once the last one leaves. No
/// per-tab ask and no line of the digest for them. The machine's due logins
/// are an Info line of the digest, NEVER an ask; no working login within a
/// day a Warn line; a renewal that signed in another account a line.
/// NEGATIVE CONTROLS: S0's stuck login after a move (`accounts login`)
/// keeps its own tab's ask; the loop's escalated login raises nothing; an
/// earlier process's renew ask no tab of this one holds ends at the settle.
#[test]
fn every_tab_stopped_on_an_ended_login_is_in_one_machine_ask() {
    use aterm_messages::need::Need;
    let mut app = App::headless_for_test();
    let told = |app: &mut App, sid: &str, standing: Option<Exception>| {
        app.note_harness_host(HostNote::ResumeException {
            sid: sid.to_string(),
            standing,
        });
    };
    let asks = |app: &App| -> Vec<String> {
        app.messages
            .live_rows()
            .filter(|row| matches!(row.msg.need(), Need::Now(_)))
            .map(|row| row.msg.key.clone().unwrap_or_default())
            .collect()
    };
    let detail = |app: &App| -> Vec<String> {
        app.messages
            .live_by_key(mr::RENEW_ASK_KEY)
            .map(|row| row.msg.detail.clone())
            .unwrap_or_default()
    };
    let lines = app.messages.digest().len();
    told(&mut app, "s-1", Some(ended_login_exception("m7-a")));
    assert_eq!(asks(&app), [mr::RENEW_ASK_KEY]);
    assert!(
        detail(&app)
            .iter()
            .any(|l| l == "Run aterm harness accounts renew m7-a"),
        "{:?}",
        detail(&app)
    );
    let id = app.messages.live_by_key(mr::RENEW_ASK_KEY).map(|r| r.id);
    told(&mut app, "s-2", Some(ended_login_exception("m7-a")));
    told(&mut app, "s-3", Some(ended_login_exception("pool-1")));
    assert_eq!(asks(&app), [mr::RENEW_ASK_KEY], "one ask, whatever stopped");
    assert!(
        detail(&app)
            .iter()
            .any(|l| l == "Run aterm harness accounts renew m7-a pool-1"),
        "{:?}",
        detail(&app)
    );
    assert!(
        app.messages
            .live_by_key(mr::RENEW_ASK_KEY)
            .is_some_and(|row| row.msg.title.contains("in 3 tabs")),
        "the tabs counted"
    );
    assert_eq!(app.messages.digest().len(), lines, "no line of the digest");
    let _ = id;
    told(&mut app, "s-1", None);
    told(&mut app, "s-2", None);
    assert_eq!(asks(&app), [mr::RENEW_ASK_KEY], "s-3 still stopped");
    assert!(
        detail(&app)
            .iter()
            .any(|l| l == "Run aterm harness accounts renew pool-1"),
        "{:?}",
        detail(&app)
    );
    told(&mut app, "s-3", None);
    assert!(asks(&app).is_empty(), "the last tab moved");

    // NEGATIVE CONTROLS: the stuck login after a move keeps its tab's ask;
    // the loop's escalation raises nothing.
    told(&mut app, "s-4", Some(stuck_login_exception()));
    assert_eq!(asks(&app), [mr::resume_ask_key("s-4")]);
    told(&mut app, "s-4", None);
    told(
        &mut app,
        "s-5",
        Some(resume_exception(ExceptionKind::Login)),
    );
    assert!(asks(&app).is_empty(), "said once, by the escalation");

    // The machine's due logins, a day's warning, a wrong account: lines.
    let listed = |due: &[&str], none_soon: bool, wrong: &[&str]| {
        HostNote::Accounts(AccountsSaid::Listed {
            signed_out: Vec::new(),
            credits: Vec::new(),
            window_full: Vec::new(),
            due: due.iter().map(|l| l.to_string()).collect(),
            none_soon,
            wrong: wrong.iter().map(|l| l.to_string()).collect(),
        })
    };
    let keys = |app: &App| -> Vec<String> {
        app.messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect()
    };
    app.note_harness_host(listed(&["m7-a"], false, &[]));
    assert_eq!(keys(&app), [mr::ACCOUNTS_DUE_KEY]);
    assert!(asks(&app).is_empty(), "a due login is a line, never an ask");
    app.note_harness_host(listed(&["m7-a"], true, &["m7-a"]));
    let mut want = vec![
        mr::ACCOUNTS_DUE_KEY.to_string(),
        mr::ACCOUNTS_NONE_SOON_KEY.to_string(),
        mr::ACCOUNTS_WRONG_KEY.to_string(),
    ];
    want.sort();
    let mut have = keys(&app);
    have.sort();
    assert_eq!(have, want);
    app.note_harness_host(listed(&[], false, &[]));
    let _ = app.settle_messages(Instant::now() + DAY);
    assert!(keys(&app).is_empty(), "renewed: every line ends");

    // An earlier process's renew ask no tab of this one holds ends at the
    // settle; one a loop tells of stands.
    let mut app = App::headless_for_test();
    app.post_message(mr::renew_ask(
        &["m7-a".to_string()],
        &["in tab 9".to_string()],
    ));
    app.ripen_exceptions(Instant::now() + DAY);
    assert!(asks(&app).is_empty());
    let mut app = App::headless_for_test();
    app.post_message(mr::renew_ask(
        &["m7-a".to_string()],
        &["in tab 9".to_string()],
    ));
    told(&mut app, "s-6", Some(ended_login_exception("m7-a")));
    app.ripen_exceptions(Instant::now() + DAY);
    assert_eq!(asks(&app), [mr::RENEW_ASK_KEY]);
}

/// A SILENT USAGE TAP IS ONE LINE FOR THE MACHINE (`HostNote::UsageTap`,
/// `harness.accounts.no-tap`): raised by the first tab that goes silent,
/// held while any tab stands silent, ended when the last is heard again or
/// its worker ends — and raised at most once a day, so a second silent tab
/// the same day, after the first was heard again, raises nothing. An `Info`
/// line, never a band row or a tab's mark. NEGATIVE CONTROL: a `silent:
/// false` for a tab never marked moves nothing.
#[test]
fn a_silent_usage_tap_is_one_line_once_a_day_and_ends_with_the_last_tab() {
    let mut app = App::headless_for_test();
    let before = glass(&app);
    let note = |app: &mut App, sid: &str, silent: bool| {
        app.note_harness_host(HostNote::UsageTap {
            sid: sid.to_string(),
            silent,
        });
        assert_eq!(glass(app), before, "nothing on the glass");
        app.messages
            .digest()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect::<Vec<_>>()
    };
    assert!(note(&mut app, "s-1", false).is_empty(), "nothing to end");
    assert_eq!(note(&mut app, "s-1", true), [mr::USAGE_TAP_SILENT_KEY]);
    assert_eq!(note(&mut app, "s-2", true), [mr::USAGE_TAP_SILENT_KEY]);
    assert_eq!(app.messages.digest().entries.len(), 1, "one line");
    assert_eq!(
        note(&mut app, "s-1", false),
        [mr::USAGE_TAP_SILENT_KEY],
        "s-2 stands"
    );
    assert!(note(&mut app, "s-2", false).is_empty(), "the last is heard");
    assert!(note(&mut app, "s-3", true).is_empty(), "once a day");
    assert!(note(&mut app, "s-3", false).is_empty());
}
