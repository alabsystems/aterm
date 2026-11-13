// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_converge::StoodAside;

fn row(outcome: Outcome, from: Option<&str>, to: Option<&str>) -> ItemReport {
    ItemReport {
        id: "item".into(),
        family: "test".into(),
        title: "item".into(),
        outcome,
        from: from.map(str::to_owned),
        to: to.map(str::to_owned),
        why: None,
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    }
}

fn planned(from: &str) -> ItemReport {
    row(Outcome::Planned, Some(from), Some("wanted"))
}

/// A planned row whose item took `reading`, under the engine's own `from` line.
fn read_as(reading: Reading) -> ItemReport {
    ItemReport {
        detail: Some(reading.token()),
        ..planned("the engine's line, which System State does not parse")
    }
}

#[test]
fn every_reading_round_trips_through_its_rows_detail() {
    let readings = [
        Reading::CapsLock,
        Reading::NaturalOn,
        Reading::UntilLogin,
        Reading::IconsHidden,
        Reading::ClickReveals,
        Reading::WidgetsShown,
        Reading::Apps {
            pinned: 7,
            wanted: 3,
        },
        Reading::SignedIn,
        Reading::DocsInstalled,
        Reading::BuildsIndexed {
            projects: 0,
            toolchain: false,
        },
        Reading::BuildsIndexed {
            projects: 3,
            toolchain: true,
        },
        Reading::KeptOut {
            projects: 1,
            toolchain: false,
        },
        Reading::KeptOut {
            projects: 0,
            toolchain: true,
        },
        Reading::Repositories {
            present: 2,
            count: 5,
            signed_in: false,
        },
        Reading::Repositories {
            present: 5,
            count: 5,
            signed_in: true,
        },
    ];
    for reading in readings
        .into_iter()
        .chain(Wait::ALL.into_iter().map(Reading::Waits))
    {
        assert_eq!(Reading::of(&read_as(reading)), Some(reading));
    }
    // Another item's detail (a public key line, a version) is no reading.
    for other in [
        "ssh-ed25519 AAAA",
        "26A428",
        "reading:apps:7",
        "reading:x",
        "reading:waits:nothing",
        "reading:waits:token:1",
    ] {
        let mut row = planned("x");
        row.detail = Some(other.into());
        assert_eq!(Reading::of(&row), None, "{other}");
    }
}

#[test]
fn every_engine_result_maps_to_the_design_standing() {
    let mut changed = row(Outcome::Changed, None, Some("x"));
    assert_eq!(standing(&changed), Standing::InPlace);
    changed.effect = Effect::Login;
    assert_eq!(standing(&changed), Standing::AtLogin);
    changed.effect = Effect::Restart;
    assert_eq!(standing(&changed), Standing::AtLogin);
    let mut blocked = row(Outcome::Blocked, None, None);
    assert_eq!(standing(&blocked), Standing::Waiting);
    // A sign-out blocked on Accessibility waits: the Accessibility row is the one
    // that asks for a person (§5: Blocked is Waiting, with its reason).
    blocked.stood_aside = Some(StoodAside::Permission("Accessibility".into()));
    assert_eq!(standing(&blocked), Standing::Waiting);
    for (outcome, want) in [
        (Outcome::Ok, Standing::InPlace),
        (Outcome::Planned, Standing::Differs),
        (Outcome::Person, Standing::NeedsYou),
        (Outcome::Deferred, Standing::Waiting),
        (Outcome::NotApplicable, Standing::NotNeeded),
        (Outcome::Unknown, Standing::Unknown),
        (Outcome::Failed, Standing::Failed),
    ] {
        assert_eq!(standing(&row(outcome, None, None)), want, "{outcome:?}");
    }
}

/// A Restore puts back a saved preference macOS loads only at the next login, while the
/// value aterm set stays live until then: the row stands as the Mac will after that login
/// (it differs from what aterm wants) and reads the live value, marked.
#[test]
fn a_value_live_until_the_next_login_reads_as_itself_marked() {
    let at_login = |from: &str| ItemReport {
        effect: Effect::Login,
        ..planned(from)
    };
    let live = row(Outcome::Ok, None, Some("Caps Lock acts as Control"));
    let saved = at_login("no remap");
    let read = observation(Setting::CapsLock, &[&saved, &live]);
    assert_eq!(read.standing, Standing::Differs);
    assert_eq!(read.now.as_deref(), Some("Control · until login"));
    // What the Mac reads after that login, which a row that will change shows as its
    // from-side: the saved mapping's word, unmarked.
    assert_eq!(read.after_login.as_deref(), Some("Caps Lock"));
    assert_eq!(read.wanted, "Control");
    // Live not as wanted, or not read at all: the saved mapping's word, and nothing else
    // after the login.
    let unmapped = read_as(Reading::CapsLock);
    for rows in [vec![&saved, &unmapped], vec![&saved]] {
        let read = observation(Setting::CapsLock, &rows);
        assert_eq!(read.now.as_deref(), Some("Caps Lock"), "{rows:?}");
        assert_eq!(read.after_login, None, "{rows:?}");
    }
    // A live part that could not be read proves nothing either.
    let unread = row(Outcome::Unknown, None, None);
    assert_ne!(
        observation(Setting::CapsLock, &[&saved, &live, &unread])
            .now
            .as_deref(),
        Some("Control · until login")
    );
    // Natural scrolling's one item reads both, and says so itself.
    let read = observation(Setting::NaturalScrolling, &[&read_as(Reading::UntilLogin)]);
    assert_eq!(read.standing, Standing::Differs);
    assert_eq!(read.now.as_deref(), Some("Off · until login"));
    assert_eq!(read.after_login.as_deref(), Some("On"));
    // Trackpad scrolling's keys are all saved preferences: one in place says nothing live.
    let gesture = at_login("false");
    let read = observation(Setting::TrackpadScrolling, &[&gesture, &live]);
    assert_eq!(read.now.as_deref(), Some("Some off"));
    assert_eq!(read.after_login, None);
}

/// A run that set the Caps Lock key's live mappings with its saved ones left nothing for
/// the next login: its live part reads in place. Trackpad scrolling has no live part, and a
/// live part not read, or not as wanted, is none in place.
#[test]
fn only_a_live_mapping_read_in_place_puts_a_row_in_effect_now() {
    let saved = ItemReport {
        effect: Effect::Login,
        ..row(Outcome::Ok, None, Some("Caps Lock→Left Control"))
    };
    let live = row(Outcome::Ok, None, Some("Caps Lock acts as Control"));
    assert!(live_in_place(Setting::CapsLock, &[&saved, &live]));
    assert!(!live_in_place(Setting::CapsLock, &[&saved]));
    for other in [Outcome::Unknown, Outcome::Planned] {
        let other = row(other, None, None);
        assert!(!live_in_place(Setting::CapsLock, &[&saved, &live, &other]));
    }
    assert!(!live_in_place(Setting::TrackpadScrolling, &[&saved, &live]));

    let mut changed_saved = saved.clone();
    changed_saved.outcome = Outcome::Changed;
    let mut changed_live = live.clone();
    changed_live.outcome = Outcome::Changed;
    let retained = readback_observation(Setting::CapsLock, &[&changed_saved, &changed_live]);
    assert_eq!(
        (retained.standing, retained.now.as_deref()),
        (Standing::InPlace, Some("Control"))
    );
    assert_eq!(changed_saved.outcome, Outcome::Changed);
    assert_eq!(changed_live.outcome, Outcome::Changed);
    assert_eq!(
        readback_observation(Setting::CapsLock, &[&changed_saved]).standing,
        Standing::AtLogin,
        "a saved-only write supplies no physical readback"
    );
    assert_eq!(
        readback_observation(Setting::CapsLock, &[&saved]).standing,
        Standing::InPlace,
        "a known-empty cohort retains saved-configuration semantics"
    );

    for outcome in [
        Outcome::Unknown,
        Outcome::Planned,
        Outcome::Blocked,
        Outcome::Failed,
        Outcome::NotApplicable,
    ] {
        let unread = row(outcome, None, None);
        let retained =
            readback_observation(Setting::CapsLock, &[&changed_saved, &changed_live, &unread]);
        assert!(
            !matches!(retained.standing, Standing::InPlace | Standing::NotNeeded),
            "a negative cohort member remains part of the reading: {outcome:?}"
        );
        assert_ne!(retained.now.as_deref(), Some("Control"));
    }
    let mut held = live.clone();
    held.stood_aside = Some(StoodAside::Permission("Accessibility".into()));
    let retained = readback_observation(Setting::CapsLock, &[&saved, &held]);
    assert_eq!((retained.standing, retained.now), (Standing::Unknown, None));
}

#[test]
fn a_row_takes_its_worst_part_and_says_its_value_only_when_read() {
    let ok = row(Outcome::Ok, None, Some("Caps Lock→Left Control"));
    let read = observation(Setting::CapsLock, &[&ok]);
    assert_eq!(read.standing, Standing::InPlace);
    assert_eq!(read.now.as_deref(), Some("Control"));
    let saved = planned("no remap");
    let live = read_as(Reading::CapsLock);
    let read = observation(Setting::CapsLock, &[&ok, &saved, &live]);
    assert_eq!(read.standing, Standing::Differs);
    assert_eq!(read.now.as_deref(), Some("Caps Lock"));
    assert_eq!(read.wanted, "Control");
    // Caps Lock mapped to something else is neither word: no value is shown.
    let other = planned("Caps Lock→Left Option");
    assert_eq!(observation(Setting::CapsLock, &[&saved, &other]).now, None);
    let seen = planned("321-9456-0: no remap; 76-0-0: Left Shift→Left Control");
    assert_eq!(
        observation(Setting::CapsLock, &[&seen]).now.as_deref(),
        Some("Caps Lock")
    );
    let unknown = row(Outcome::Unknown, None, None);
    let read = observation(Setting::CapsLock, &[&saved, &unknown]);
    assert_eq!(read.standing, Standing::Differs);
    let read = observation(Setting::CapsLock, &[&unknown]);
    assert_eq!((read.standing, read.now), (Standing::Unknown, None));
    let skipped = row(Outcome::NotApplicable, None, None);
    let read = observation(Setting::Spotlight, &[&skipped, &skipped]);
    assert_eq!((read.standing, read.now), (Standing::NotNeeded, None));
    assert_eq!(observation(Setting::Dock, &[]).standing, Standing::Unknown);
}

#[test]
fn each_setting_names_only_the_part_it_read() {
    let apps = |pinned| Reading::Apps { pinned, wanted: 3 };
    let cases: [(Setting, &[Reading], Option<&str>); 12] = [
        (Setting::NaturalScrolling, &[Reading::NaturalOn], Some("On")),
        (Setting::NaturalScrolling, &[], None),
        (
            Setting::Desktop,
            &[Reading::IconsHidden],
            Some("Icons hidden"),
        ),
        (Setting::Desktop, &[], None),
        (Setting::Widgets, &[Reading::WidgetsShown], Some("Shown")),
        (Setting::Dock, &[apps(7)], Some("7 apps")),
        (Setting::Dock, &[apps(1)], Some("1 app")),
        // Three other apps never read as the wanted "3 apps".
        (Setting::Dock, &[apps(3)], Some("Other apps")),
        (Setting::Messages, &[Reading::SignedIn], Some("Signed in")),
        (Setting::FaceTime, &[], None),
        (
            Setting::Spotlight,
            &[Reading::DocsInstalled],
            Some("Docs indexed"),
        ),
        (
            Setting::Spotlight,
            &[Reading::BuildsIndexed {
                projects: 0,
                toolchain: false,
            }],
            Some("Builds indexed"),
        ),
    ];
    for (setting, readings, want) in cases {
        let mut rows: Vec<ItemReport> = readings.iter().map(|r| read_as(*r)).collect();
        if rows.is_empty() {
            rows.push(planned("a reading with no word"));
        }
        let refs: Vec<&ItemReport> = rows.iter().collect();
        let read = observation(setting, &refs);
        assert_eq!(read.standing, Standing::Differs, "{setting:?} {readings:?}");
        assert_eq!(read.now.as_deref(), want, "{setting:?} {readings:?}");
        // The engine's `from` line under the reading is no description.
        assert_eq!(read.detail, None, "{setting:?} {readings:?}");
    }
    // Spotlight names the part that read a word.
    let profile = planned("rustup's profile is default, which installs rust-docs");
    let docs = read_as(Reading::DocsInstalled);
    let read = observation(Setting::Spotlight, &[&profile, &docs]);
    assert_eq!(read.now.as_deref(), Some("Docs indexed"));
    // Trackpad gestures read the defaults items' rendered values.
    let (absent, off, on) = (planned("absent"), planned("false"), planned("1"));
    let read = observation(Setting::TrackpadScrolling, &[&absent, &off]);
    assert_eq!(read.now.as_deref(), Some("Some off"));
    assert_eq!(
        observation(Setting::TrackpadScrolling, &[&absent, &on]).now,
        None
    );
    // Parts that disagree name no value.
    let (seven, one) = (read_as(apps(7)), read_as(apps(1)));
    assert_eq!(observation(Setting::Dock, &[&seven, &one]).now, None);
}

/// What Spotlight still indexes that the row cannot change itself is a quiet note,
/// never a step: the person's own project build folders, and the toolchain build aterm
/// is using, which atpkg moves at its next update.
#[test]
fn what_spotlight_still_indexes_is_a_note_never_a_step() {
    let kept = |projects: usize, toolchain: bool| ItemReport {
        detail: Some(
            Reading::KeptOut {
                projects,
                toolchain,
            }
            .token(),
        ),
        ..row(
            Outcome::Ok,
            None,
            Some(
                "cargo-build.noindex and tooling.noindex are .noindex; Spotlight indexes these project build folders of yours, which aterm leaves as they are: /Users//a/x/target",
            ),
        )
    };
    let profile = row(Outcome::Ok, None, None);
    let read = observation(Setting::Spotlight, &[&profile, &kept(3, false)]);
    assert_eq!(read.standing, Standing::InPlace);
    assert_eq!(
        read.note.as_deref(),
        Some(
            "Spotlight indexes 3 of your project build folders; renaming each to its .noindex name and linking the old name to it keeps it out"
        )
    );
    assert!(
        read.detail
            .as_deref()
            .is_some_and(|d| d.contains("/Users//a/x/target")),
        "the list stands behind the row: {read:?}"
    );
    assert_eq!(
        projects_note(1),
        "Spotlight indexes 1 of your project build folders; renaming it to its .noindex name and linking the old name to it keeps it out"
    );
    // The toolchain build aterm is using, which atpkg moves: said first, asked of no one.
    let read = observation(Setting::Spotlight, &[&kept(0, true)]);
    assert_eq!(read.standing, Standing::InPlace);
    assert_eq!(
        read.note.as_deref(),
        Some("aterm moves the Rust toolchain it is using out of Spotlight at its next update")
    );
    let read = observation(Setting::Spotlight, &[&kept(2, true)]);
    assert_eq!(
        read.note.as_deref(),
        Some(
            "aterm moves the Rust toolchain it is using out of Spotlight at its next update; Spotlight indexes 2 of your project build folders; renaming each to its .noindex name and linking the old name to it keeps it out"
        )
    );
    // A change aterm makes says it too; none counted says nothing.
    let read = observation(
        Setting::Spotlight,
        &[&read_as(Reading::BuildsIndexed {
            projects: 2,
            toolchain: false,
        })],
    );
    assert_eq!(read.standing, Standing::Differs);
    assert!(
        read.note
            .is_some_and(|note| note.starts_with("Spotlight indexes 2 "))
    );
    let read = observation(Setting::Spotlight, &[&kept(0, false)]);
    assert_eq!((read.standing, read.note), (Standing::InPlace, None));
}

#[test]
fn repositories_count_what_is_present_and_want_all_of_them() {
    let partial = read_as(Reading::Repositories {
        present: 3,
        count: 7,
        signed_in: false,
    });
    let read = observation(Setting::Repositories, &[&partial]);
    assert_eq!(read.now.as_deref(), Some("3 of 7"));
    assert_eq!(read.wanted, "All 7");
    let sign_in = read_as(Reading::Repositories {
        present: 7,
        count: 7,
        signed_in: false,
    });
    let read = observation(Setting::Repositories, &[&sign_in]);
    assert_eq!(read.now.as_deref(), Some("Needs sign-in"));
    assert_eq!(read.wanted, "All 7");
    let ok = ItemReport {
        detail: Some(
            Reading::Repositories {
                present: 7,
                count: 7,
                signed_in: true,
            }
            .token(),
        ),
        ..row(Outcome::Ok, None, Some("7 matching worktrees present"))
    };
    let read = observation(Setting::Repositories, &[&ok]);
    assert_eq!(read.now.as_deref(), Some("All 7"));
    let mut blocked = row(Outcome::Blocked, None, None);
    blocked.why = Some("offline".into());
    let read = observation(Setting::Repositories, &[&blocked]);
    assert_eq!(read.standing, Standing::Waiting);
    assert_eq!((read.now, read.wanted.as_str()), (None, "All"));
    assert_eq!(read.detail.as_deref(), Some("offline"));
}

#[test]
fn successful_repository_operations_need_a_complete_signed_in_readback() {
    for outcome in [Outcome::Ok, Outcome::Changed] {
        for (present, count, signed_in, expected) in [
            (0, 53, true, Standing::Differs),
            (17, 53, true, Standing::Differs),
            (53, 53, false, Standing::Differs),
            (53, 53, true, Standing::InPlace),
            (0, 0, true, Standing::InPlace),
        ] {
            let report = ItemReport {
                detail: Some(
                    Reading::Repositories {
                        present,
                        count,
                        signed_in,
                    }
                    .token(),
                ),
                ..row(outcome, None, None)
            };
            let observed = observation(Setting::Repositories, &[&report]);
            assert_eq!(observed.standing, expected, "{report:?}");
            if present < count {
                assert_eq!(observed.now, Some(format!("{present} of {count}")));
                assert!(!observed.standing.settled());
            }
        }
    }
}

/// A waiting row's note is the reason of the first waiting part whose item decided one, and a
/// row only a person moves says their remedy (design §3.6), never the engine's line; a row
/// with no such part, or one that neither waits nor needs a person, gives no note.
#[test]
fn a_row_notes_the_reason_or_remedy_its_item_decided_and_never_the_engine_line() {
    let blocked = |wait: Option<Wait>, why: &str| ItemReport {
        why: Some(why.to_owned()),
        detail: wait.map(|wait| Reading::Waits(wait).token()),
        ..row(Outcome::Blocked, None, None)
    };
    let line = "GitHub token source must be an owned private regular file (0600), without \
                links, at most 64 KiB";
    for wait in Wait::ALL {
        // As the item reports it: a wait that ends by itself is blocked, one only a person
        // ends is theirs.
        let person = wait.kind() == FailureKind::NeedsPerson;
        let mut part = blocked(Some(wait), line);
        if person {
            part.outcome = Outcome::Person;
        }
        let read = observation(Setting::Repositories, &[&part]);
        let standing = if person {
            Standing::NeedsYou
        } else {
            Standing::Waiting
        };
        assert_eq!(read.standing, standing, "{wait:?}");
        assert_eq!(read.note.as_deref(), Some(wait.note()), "{wait:?}");
        let now = matches!(wait, Wait::Token | Wait::TokenRefused | Wait::OtherAccount)
            .then_some("Needs sign-in");
        assert_eq!(
            (read.now.as_deref(), read.wanted.as_str()),
            (now, "All"),
            "{wait:?}"
        );
        // The engine's line stays the description, for `aterm init` and the doctor.
        assert_eq!(read.detail.as_deref(), Some(line), "{wait:?}");
    }
    // The first waiting part that says why gives the note; a planned part beside it
    // and a waiting part that says nothing do not.
    let silent = blocked(None, "waits for init.github.auth@alice");
    let planned = read_as(Reading::Repositories {
        present: 1,
        count: 2,
        signed_in: true,
    });
    let read = observation(
        Setting::Repositories,
        &[&planned, &silent, &blocked(Some(Wait::Unreached), line)],
    );
    assert_eq!(read.note.as_deref(), Some(Wait::Unreached.note()));
    let read = observation(Setting::Repositories, &[&planned, &silent]);
    assert_eq!((read.standing, read.note), (Standing::Waiting, None));
    // A row that does not wait has no note, whatever its parts read.
    let mut failed = blocked(Some(Wait::Unreached), line);
    failed.outcome = Outcome::Failed;
    let read = observation(Setting::Repositories, &[&failed]);
    assert_eq!((read.standing, read.note), (Standing::Failed, None));
}

/// A step that failed gives the reason it decided ([`failure_note`]), the page's words for
/// it; a failed part that decided none, and a part that only waits, give none.
#[test]
fn a_failed_part_gives_the_reason_its_step_decided() {
    let blocked = |wait: Option<Wait>, why: &str| ItemReport {
        why: Some(why.to_owned()),
        detail: wait.map(|wait| Reading::Waits(wait).token()),
        ..row(Outcome::Blocked, None, None)
    };
    let line = "GET https://api.github.com/user: connection refused";
    let mut failed = blocked(Some(Wait::Unreached), line);
    failed.outcome = Outcome::Failed;
    assert_eq!(failure_note(&[&failed]), Some(Wait::Unreached.note()));
    let mut silent = blocked(None, line);
    silent.outcome = Outcome::Failed;
    assert_eq!(failure_note(&[&silent]), None);
    assert_eq!(
        failure_note(&[&blocked(Some(Wait::Unreached), line)]),
        None,
        "a wait is no failure"
    );
    assert_eq!(
        failure_note(&[&silent, &failed]),
        Some(Wait::Unreached.note())
    );
}

/// A row that differs has no description (design §3.3): a planned part's `from → to` and its
/// `why` are the engine's; a row of another standing takes its worst parts' reasons.
#[test]
fn a_row_that_differs_carries_no_engine_line_as_its_description() {
    let gestures = [
        planned("absent"),
        planned("false"),
        ItemReport {
            why: Some("read after workstation.x, which this plan changes first (y)".into()),
            ..planned("absent")
        },
    ];
    let read = observation(
        Setting::TrackpadScrolling,
        &gestures.iter().collect::<Vec<_>>(),
    );
    assert_eq!(read.standing, Standing::Differs);
    assert_eq!(read.detail, None, "{read:?}");
    let dock = row(
        Outcome::Planned,
        Some("com.apple.dock persistent-apps, com.apple.dock show-recents"),
        Some("Dock pins: aterm"),
    );
    assert_eq!(observation(Setting::Dock, &[&dock]).detail, None);
    // A waiting row gives its reason, not a planned part's line beside it,
    // and a blocked part with no reason gives none.
    let mut offline = row(Outcome::Blocked, Some("absent"), Some("true"));
    offline.why = Some("offline".into());
    let read = observation(Setting::TrackpadScrolling, &[&gestures[0], &offline]);
    assert_eq!(read.standing, Standing::Waiting);
    assert_eq!(read.detail.as_deref(), Some("offline"));
    let silent = row(Outcome::Blocked, Some("absent"), Some("true"));
    assert_eq!(
        observation(Setting::TrackpadScrolling, &[&silent]).detail,
        None
    );
    // A failure's reason stays its description.
    let mut failed = row(Outcome::Failed, Some("absent"), Some("true"));
    failed.why = Some("applied, but it still reads absent".into());
    assert_eq!(
        observation(Setting::TrackpadScrolling, &[&failed])
            .detail
            .as_deref(),
        Some("applied, but it still reads absent")
    );
}
