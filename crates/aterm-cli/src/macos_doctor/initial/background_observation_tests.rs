// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn rows(settings: &[Setting]) -> Selection {
    let mut rows = Selection::none();
    for setting in settings {
        rows.set(*setting, true);
    }
    rows
}

fn caps() -> Selection {
    rows(&[Setting::CapsLock])
}

/// A reply of this schema with `setup`, `verified`, `verified_at` and `rows`, beside the
/// live keyboard diagnostic and the lifecycle fields the window publishes too: `caps_live`
/// says every HID service maps Caps Lock to Control, which never verifies the saved
/// setting, and no lifecycle field decides a row, so every case here holds with them.
fn reply(setup: &str, verified: Selection, at: &str, said: &str) -> String {
    format!(
        "OK schema={SETUP_STATUS_SCHEMA} sid=0 setup={setup} verified={} verified_at={at} \
         observed_generation=7 rows={said} caps_live=control busy=0 finished=1 queued=0 \
         changing=0 retrying=0 running=-",
        verified.bits()
    )
}

/// A row asked and not answered (the ALab vault off its LAN) heads `setup=` as `asking:`,
/// every other row settled: the reply is evidence, and the rows a check asks about decide it
/// as for `done`, with `done`'s tails. Before schema 6 that head was no grammar, so a Mac off
/// the ALab LAN gave no usable reply. NEGATIVE CONTROL: an `asking:` head naming a row `rows=` does not say
/// `asking` says two things of one reading, which is no evidence.
#[test]
fn an_asking_head_is_settled_evidence_of_the_rows_beside_it() {
    let read_back = ReadBack {
        rows: caps(),
        read_at: Some(1_700_000_000),
        ..ReadBack::default()
    };
    for setup in [
        "asking:vault",
        "asking:vault;read-back:capslock",
        "asking:vault;asking:accounts;offered:fda",
    ] {
        assert_eq!(
            observation(
                &reply(
                    setup,
                    caps(),
                    "1700000000",
                    "capslock:in-place,vault:asking,fda:offered"
                ),
                caps()
            ),
            WorkstationObservation::Complete(read_back.clone()),
            "{setup}"
        );
    }
    // Schema 6's two additions in one reply: a row changed since aterm set it is named
    // `;changed:` after an `asking:` head as after `done`.
    assert_eq!(
        observation(
            &reply(
                "asking:vault;changed:capslock",
                caps(),
                "1700000000",
                "capslock:differs,vault:asking"
            ),
            caps()
        ),
        WorkstationObservation::Complete(ReadBack {
            changed: caps(),
            ..read_back.clone()
        })
    );
    for said in [
        "capslock:in-place,vault:waiting",
        "capslock:in-place,vault:in-place",
    ] {
        assert!(
            matches!(
                observation(&reply("asking:vault", caps(), "1700000000", said), caps()),
                WorkstationObservation::Unavailable(_)
            ),
            "{said}"
        );
    }
    assert!(matches!(
        observation(
            &reply(
                "asking:vault;needs:vault",
                caps(),
                "1700000000",
                "vault:asking"
            ),
            caps()
        ),
        WorkstationObservation::Unavailable(_)
    ));
}

/// Only the rows a check asks about decide it: a row read back settled completes it
/// whatever else the coordinator still does, a press it waits on is a permission, any other
/// step left a wait, and a row with no usable reading unread, neither a wait nor done.
#[test]
fn background_observation_keeps_completion_waits_and_permissions_distinct() {
    let read_back = ReadBack {
        rows: caps(),
        read_at: Some(1_700_000_000),
        ..ReadBack::default()
    };
    for setup in ["done", "pending:repos", "pending:repos;needs:repos"] {
        let said = if setup.contains("needs") {
            "capslock:in-place,repos:needs-you"
        } else {
            "capslock:in-place,repos:waiting"
        };
        assert_eq!(
            observation(&reply(setup, caps(), "1700000000", said), caps()),
            WorkstationObservation::Complete(read_back.clone()),
            "{setup}: rows it does not ask about say nothing of it"
        );
    }
    for (setup, said) in [
        ("pending:capslock", "capslock:differs,dock:unread"),
        ("pending:capslock", "capslock:waiting"),
        ("pending:capslock", "capslock:failed"),
        ("pending:capslock", "capslock:unknown"),
        // A change under way that could move it verifies nothing.
        ("pending", "capslock:in-place"),
        ("pending;unread:dock", "capslock:at-login,dock:unread"),
    ] {
        let WorkstationObservation::Waiting(why) =
            observation(&reply(setup, Selection::none(), "-", said), caps())
        else {
            panic!("{setup} {said}: the asked row has a step left");
        };
        assert!(why.contains("capslock") && !why.contains("dock"), "{why}");
    }
    assert!(matches!(
        observation(
            &reply(
                "pending:capslock;needs:capslock",
                Selection::none(),
                "-",
                "capslock:needs-you"
            ),
            caps()
        ),
        WorkstationObservation::Permission(_)
    ));
    assert_eq!(
        observation(
            &reply("unread:capslock", Selection::none(), "-", "capslock:unread"),
            caps()
        ),
        WorkstationObservation::Unread {
            rows: "capslock".into(),
            read_back: ReadBack::default(),
        }
    );
    // A row the coordinator does not manage is one it neither reads nor sets.
    assert_eq!(
        observation(&reply("done", Selection::none(), "-", "-"), caps()),
        WorkstationObservation::Unread {
            rows: "capslock".into(),
            read_back: ReadBack::default(),
        }
    );
}

/// The writer says a row read back past its standing only where it verifies it, and verifies
/// nothing while a change that could move it is under way (`aterm_gui`'s `row_said`): every
/// such answer is a wait, never missing evidence (the review's blocking finding, where
/// `pending;read-back:dock verified=0` read as unavailable and `aterm init --check` exited 1).
#[test]
fn a_pass_under_way_is_a_wait_for_the_rows_it_could_move() {
    let dock = rows(&[Setting::Dock]);
    for setup in ["pending", "pending:repos", "pending;unread:capslock"] {
        let said = if setup.contains("repos") {
            "dock:in-place,repos:waiting,capslock:in-place"
        } else if setup.contains("unread") {
            "dock:in-place,capslock:unread"
        } else {
            "dock:in-place"
        };
        assert!(
            matches!(
                observation(&reply(setup, Selection::none(), "-", said), dock),
                WorkstationObservation::Waiting(_)
            ),
            "{setup}"
        );
    }
    // NEGATIVE CONTROL: a row said read back that is not verified is two things of one
    // reading, which no writer of this schema says.
    assert!(matches!(
        observation(
            &reply(
                "pending;read-back:dock",
                Selection::none(),
                "-",
                "dock:in-place"
            ),
            dock
        ),
        WorkstationObservation::Unavailable(_)
    ));
}

/// Grants are asked about only with the Permissions rows: one on offer is neither granted
/// nor a person's step, and is said beside the rows read back (the review found the CLI
/// dropped `;offered:`, so `aterm status` read such a Mac set up now); one this process
/// cannot read is unread, never a wait (it once read `pending:fda`, exit 75 for good).
#[test]
fn grants_on_offer_and_unread_grants_are_said_as_they_are() {
    let mut with_grants = caps();
    with_grants.set(Setting::Permissions, true);
    let offered = reply(
        "done;offered:fda,debugger",
        caps(),
        "1700000000",
        "fda:offered,devtools:in-place,debugger:offered,capslock:in-place",
    );
    assert_eq!(
        observation(&offered, with_grants),
        WorkstationObservation::Complete(ReadBack {
            rows: caps(),
            read_at: Some(1_700_000_000),
            offered: vec![Access::FullDiskAccess, Access::Debugger],
            ..ReadBack::default()
        })
    );
    let WorkstationObservation::Complete(said) = observation(&offered, with_grants) else {
        panic!("on offer is no step left");
    };
    assert!(
        said.notes()
            .contains("Full Disk Access, Debugger access on offer"),
        "{}",
        said.notes()
    );
    assert_eq!(
        observation(&offered, caps()),
        WorkstationObservation::Complete(ReadBack {
            rows: caps(),
            read_at: Some(1_700_000_000),
            ..ReadBack::default()
        }),
        "a check that asks about no grant hears of none"
    );
    let unread = reply(
        "unread:fda",
        caps(),
        "1700000000",
        "fda:unread,capslock:in-place",
    );
    assert_eq!(
        observation(&unread, with_grants),
        WorkstationObservation::Unread {
            rows: "fda".into(),
            read_back: ReadBack {
                rows: caps(),
                read_at: Some(1_700_000_000),
                ..ReadBack::default()
            },
        }
    );
    assert!(matches!(
        observation(&unread, caps()),
        WorkstationObservation::Complete(_)
    ));
    assert!(matches!(
        observation(
            &reply(
                "pending:fda;needs:fda",
                Selection::none(),
                "-",
                "fda:needs-you"
            ),
            with_grants
        ),
        WorkstationObservation::Permission(_)
    ));
}

/// A row set for the next login or changed since aterm set it is read back settled, and
/// says which it is; one read back past a reading's standing is a completion dated when it
/// was read, so a finished Mac nobody watches is never read as unread forever.
#[test]
fn rows_read_back_settled_at_any_age_are_a_dated_completion() {
    let asked = rows(&[Setting::CapsLock, Setting::TrackpadScrolling, Setting::Dock]);
    assert_eq!(
        observation(
            &reply(
                "read-back:capslock;at-login:trackpad;changed:dock",
                asked,
                "1700000000",
                "capslock:in-place,trackpad:at-login,dock:differs",
            ),
            asked
        ),
        WorkstationObservation::Complete(ReadBack {
            rows: asked,
            read_at: Some(1_700_000_000),
            at_login: rows(&[Setting::TrackpadScrolling]),
            changed: rows(&[Setting::Dock]),
            ..ReadBack::default()
        })
    );
    // One row with no reading beside them is unread, and carries what was read back.
    let mut with_dock = caps();
    with_dock.set(Setting::Dock, true);
    assert_eq!(
        observation(
            &reply(
                "unread:dock;read-back:capslock",
                caps(),
                "1700000000",
                "capslock:in-place,dock:unread",
            ),
            with_dock
        ),
        WorkstationObservation::Unread {
            rows: "dock".into(),
            read_back: ReadBack {
                rows: caps(),
                read_at: Some(1_700_000_000),
                ..ReadBack::default()
            },
        }
    );
}

/// An update put this command on disk before the running aterm relaunched into it: its
/// older reply is read as nothing but a wait that ends once it does, never as missing
/// evidence and never as the done or the rows it would claim (schema 5's `;yours:` named rows
/// changed since aterm set them as the person's). NEGATIVE CONTROL: the current schema's same
/// done is read as done, and a newer schema is no evidence at all.
#[test]
fn an_older_coordinator_is_a_wait_until_it_relaunches() {
    for reply in [
        "OK schema=5 setup=done;yours:dock verified=8 verified_at=1700000000 \
         observed_generation=7 rows=dock:differs",
        "OK schema=4 setup=done verified=255 verified_at=1700000000",
        "OK schema=3 setup=unread:dock verified=0 verified_at=-",
        "OK schema=2 setup=done verified=255 observed_generation=7 rows=capslock:in-place",
        "OK schema=2 setup=pending:dock verified=0",
        "OK schema=1",
    ] {
        assert!(
            matches!(
                observation(reply, caps()),
                WorkstationObservation::Waiting(_)
            ),
            "{reply}"
        );
    }
    assert!(matches!(
        observation(
            &reply("done", caps(), "1700000000", "capslock:in-place"),
            caps()
        ),
        WorkstationObservation::Complete(_)
    ));
    assert!(matches!(
        observation(
            &format!(
                "OK schema={} setup=done verified=1 verified_at=1700000000 \
                 observed_generation=7 rows=capslock:in-place",
                SETUP_STATUS_SCHEMA + 1
            ),
            caps()
        ),
        WorkstationObservation::Unavailable(_)
    ));
}

#[test]
fn missing_refused_or_ambiguous_evidence_is_never_completion_or_permission() {
    let full = |rest: &str| format!("OK schema={SETUP_STATUS_SCHEMA} {rest}");
    let mut replies: Vec<String> = [
        "ERR auth",
        "OK schema=three setup=done verified=0 verified_at=- observed_generation=1 rows=-",
        "OK setup=done verified=0 verified_at=- observed_generation=1 rows=-",
    ]
    .map(str::to_owned)
    .to_vec();
    replies.extend(
        [
            "schema=5 setup=done verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=done verified=0 verified_at=- observed_generation=1",
            "setup=done verified=0 verified_at=- rows=-",
            "setup=done verified=0 observed_generation=1 rows=-",
            "setup=done verified=0 verified_at=- observed_generation=soon rows=-",
            "setup=done verified=0 verified_at=- observed_generation=1 rows=- rows=-",
            "setup=done verified=0 verified=1 verified_at=- observed_generation=1 rows=-",
            "setup=done verified=1 verified_at=- observed_generation=1 rows=capslock:in-place",
            "setup=done verified=0 verified_at=1700000000 observed_generation=1 rows=-",
            "setup=done verified=1 verified_at=soon observed_generation=1 rows=capslock:in-place",
            "setup=done verified=1 verified_at=0 observed_generation=1 rows=capslock:in-place",
            // Permissions and beyond are no private reading.
            "setup=done verified=512 verified_at=1 observed_generation=1 rows=-",
            "setup=done verified=65536 verified_at=1 observed_generation=1 rows=-",
            "setup=done verified=-1 verified_at=- observed_generation=1 rows=-",
            // A row no row has, said twice, with no standing, or a standing no row has.
            "setup=done verified=0 verified_at=- observed_generation=1 rows=lan:in-place",
            "setup=done verified=0 verified_at=- observed_generation=1 \
             rows=capslock:in-place,capslock:unread",
            "setup=done verified=0 verified_at=- observed_generation=1 rows=capslock:",
            "setup=done verified=0 verified_at=- observed_generation=1 rows=capslock:invented",
            "setup=done verified=0 verified_at=- observed_generation=1 rows=capslock",
            // A verified row `rows=` does not read settled.
            "setup=done verified=1 verified_at=1 observed_generation=1 rows=capslock:unread",
            "setup=done verified=1 verified_at=1 observed_generation=1 rows=capslock:waiting",
            "setup=done verified=1 verified_at=1 observed_generation=1 rows=dock:in-place",
            // `setup=` saying what `rows=` and `verified` do not.
            "setup=unread:capslock verified=0 verified_at=- observed_generation=1 \
             rows=capslock:unknown",
            "setup=done;offered:fda verified=0 verified_at=- observed_generation=1 \
             rows=fda:not-needed",
            "setup=pending:fda;needs:fda verified=0 verified_at=- observed_generation=1 \
             rows=fda:waiting",
            "setup=read-back:dock verified=0 verified_at=- observed_generation=1 \
             rows=dock:in-place",
            // The grammar of `setup=` itself.
            "setup=- verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=unread verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=unread: verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=read-back verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=done setup=pending verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=done;needs:dock verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=done;unread:dock verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=pending:repos;needs:dock verified=0 verified_at=- observed_generation=1 \
             rows=-",
            "setup=pending;needs:dock verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=pending:repos;asking:vault verified=0 verified_at=- observed_generation=1 \
             rows=-",
            "setup=pending:repos;at-login:trackpad verified=0 verified_at=- \
             observed_generation=1 rows=-",
            "setup=done;offered:fda;offered:fda verified=0 verified_at=- \
             observed_generation=1 rows=-",
            "setup=done;asking: verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=pending:repos,,vault verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=done;changed:vault verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=done;yours:dock verified=0 verified_at=- observed_generation=1 rows=-",
            "setup=done verified=0 verified_at=- observed_generation=1 rows=-\nERR failed",
        ]
        .map(full),
    );
    for reply in replies {
        assert!(
            matches!(
                observation(&reply, Selection::all()),
                WorkstationObservation::Unavailable(_)
            ),
            "{reply}"
        );
    }
}
