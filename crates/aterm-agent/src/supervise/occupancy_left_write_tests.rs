// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for a line left in the composer at a break's door (the slice-2 plan's 2.9b): a rule's
//! line at slice 1's hold door and a wake's at 2.8's break door, each written and its Enter not
//! gone (`Written::Left`), settled at the session's next look, the same way: sent (the session's
//! submit count risen, or a turn seen running with the composer clear where no count is served),
//! told; cleared unsent, released, a wake's claim given back; the session gone, released.

use super::*;

/// The session's submit count on every read from now on.
fn submits(s: &mut Session<'_, Logged<'_>>, n: u64) {
    s.ctl.m.submits_on = (0..64).map(|i| (i, n)).collect();
}

/// The first Enter after the write is refused: a person keyed before it, the line left.
fn enter_refused(m: &mut Mock) {
    m.key_replies
        .push_back(ok("OK skipped reason=person seq=101\n"));
}

/// The session's reads from now on draw `line` standing in its composer, the cursor after it (a
/// draft, never a placeholder the next keystroke replaces).
fn standing(s: &mut Session<'_, Logged<'_>>, line: &str) {
    s.ctl.m.screens = vec![drafted(break_screen(), line)];
    s.ctl.m.served = 0;
    s.ctl.m.screen_cols.insert(0, 12);
}

/// The decisions and reasons of the run's `fact-hold@v1` outcome rows (its left row among them),
/// oldest first.
fn hold_rows(ran: &Ran) -> Vec<(String, String)> {
    ran.rows
        .iter()
        .filter(|(rule, ..)| rule == HOLD)
        .map(|(_, decision, _, reason)| (decision.clone(), reason.clone()))
        .collect()
}

/// The last word of `reason`.
fn last_word(reason: &str) -> &str {
    reason.rsplit(' ').next().unwrap_or("")
}

/// How a hold door's left line is met at the next break (32 minutes in, a minute after it was
/// left): `count` the session's submit count there (`None`, a host that serves none), the
/// composer still holding the line where `stands`, and what `then` does first.
#[derive(Clone, Copy)]
struct Look {
    count: Option<u64>,
    stands: bool,
    then: fn(&mut Session<'_, Logged<'_>>),
}

const AT_BREAK: Look = Look {
    count: Some(5),
    stands: false,
    then: |_| {},
};

/// A rule's line written at 31 minutes, its Enter refused, then met as `look` says at 32 and the
/// session read again at `more` (seconds) after.
fn hold_left(tag: &str, look: Look, more: &[i64], fault: Option<Fault>) -> Ran {
    let line = templates::wait_nudge(31 * MIN);
    let mut at_s = vec![0, 31 * 60, 32 * 60];
    at_s.extend_from_slice(more);
    let script = Script {
        set: &enter_refused,
        ..Script::new(tag, Occupancy::Prompt, &at_s)
    };
    let run = || {
        script.run(&mut |s, i| {
            if look.count.is_some() && i == 0 {
                submits(s, 5);
            }
            if i == 2 {
                if look.stands {
                    standing(s, line.text());
                }
                if let Some(n) = look.count {
                    submits(s, n);
                }
                (look.then)(s);
            }
        })
    };
    match fault {
        Some(f) => fault::with(f, run),
        None => run(),
    }
}

/// A RULE'S LINE LEFT IN THE COMPOSER IS NEITHER TOLD NOR UNTOLD UNTIL THE NEXT LOOK: its
/// `fact-hold@v1 waiting … left submits=<n>` row after the intent (the count the read before the
/// Enter saw), nothing typed at the door again while it stands, and nothing told to the host.
/// The session's next read settles it: the count risen since (the person's Enter), told
/// (`typed … settled=submitted`, the host told the line then); the composer clear and the count
/// where it was, released (`skipped … why=cleared`), and the rule tried again past the session's
/// pace; the line still standing, nothing settles.
#[test]
fn a_rules_line_left_in_the_composer_is_settled_at_the_next_break() {
    let line = templates::wait_nudge(31 * MIN);
    let sent = hold_left(
        "left-hold-sent",
        Look {
            count: Some(6),
            ..AT_BREAK
        },
        &[60 * 60],
        None,
    );
    let rows = hold_rows(&sent);
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert_eq!(rows[0].0, "waiting");
    assert!(rows[0].1.ends_with(" left submits=5"), "{rows:#?}");
    assert_eq!(rows[1].0, "typed");
    assert!(rows[1].1.ends_with(" settled=submitted"), "{rows:#?}");
    assert_eq!(sent.writes().len(), 2, "one line: {:#?}", sent.writes());
    assert_eq!(
        sent.calls
            .iter()
            .filter(|c| c.starts_with("inform "))
            .collect::<Vec<_>>(),
        [&format!("inform {}", line.text())],
        "the host told once, at the settle"
    );
    let cleared = hold_left("left-hold-cleared", AT_BREAK, &[60 * 60], None);
    let rows = hold_rows(&cleared);
    let words: Vec<(&str, &str)> = rows
        .iter()
        .take(2)
        .map(|(d, r)| (d.as_str(), last_word(r)))
        .collect();
    assert_eq!(
        words,
        [("waiting", "submits=5"), ("skipped", "why=cleared")],
        "{rows:#?}"
    );
    assert!(
        rows.len() == 3 && rows[2].0 == "typed" && !rows[2].1.contains(" settled="),
        "released, then typed again past the pace: {rows:#?}"
    );
    assert_eq!(cleared.writes().len(), 4, "{:#?}", cleared.writes());
    let stands = hold_left(
        "left-hold-stands",
        Look {
            stands: true,
            count: Some(6),
            ..AT_BREAK
        },
        &[],
        None,
    );
    let rows = hold_rows(&stands);
    assert_eq!(rows.len(), 1, "nothing settles while it stands: {rows:#?}");
    assert!(rows[0].1.ends_with(" left submits=5"), "{rows:#?}");
    assert_eq!(stands.writes().len(), 2, "{:#?}", stands.writes());
    assert!(
        !stands.calls.iter().any(|c| c.starts_with("inform ")),
        "{:#?}",
        stands.calls
    );
}

/// ON A HOST THAT SERVES NO SUBMIT COUNT A TURN SEEN RUNNING WITH THE COMPOSER CLEAR IS THE LINE
/// GONE: the live read after the left write settles it told (`settled=turn`). NEGATIVE CONTROL:
/// the same host's break read with the composer clear and no turn seen releases it.
#[test]
fn a_turn_that_ran_with_the_composer_clear_tells_the_left_line() {
    let turn = hold_left(
        "left-hold-turn",
        Look {
            count: None,
            stands: false,
            then: |s| {
                let screen = s.screen().expect("a live read");
                s.took(
                    screen.seq,
                    Some(&policy(Occupancy::Prompt)),
                    &mut Heard::default(),
                );
            },
        },
        &[],
        None,
    );
    let rows = hold_rows(&turn);
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert!(
        rows[0].1.ends_with(" left") && rows[1].0 == "typed",
        "{rows:#?}"
    );
    assert!(rows[1].1.ends_with(" settled=turn"), "{rows:#?}");
    let control = hold_left(
        "left-hold-turn-control",
        Look {
            count: None,
            ..AT_BREAK
        },
        &[],
        None,
    );
    let rows = hold_rows(&control);
    assert!(
        rows.len() == 2 && rows[1].0 == "skipped" && rows[1].1.ends_with(" why=cleared"),
        "CONTROL: {rows:#?}"
    );
}

/// THE SESSION GONE RELEASES THE LINE LEFT IN ITS COMPOSER (`skipped … why=gone`): the loop's
/// end on a session that went (`Session::drive`) is the look that settles it, and no hold's end
/// row is booked for a session gone. CONTROL: the loop's end where the session lives on makes no
/// read and releases nothing: the line stays left, as `Session::loop_ended` keeps it.
#[test]
fn a_session_gone_releases_the_line_left_in_its_composer() {
    let end = |tag: &str, gone: bool| {
        let script = Script {
            set: &enter_refused,
            gone,
            ..Script::new(tag, Occupancy::Prompt, &[0, 31 * 60])
        };
        let ran = script.run(&mut |s, i| {
            if i == 0 {
                submits(s, 5);
            }
        });
        hold_rows(&ran)
    };
    let rows = end("left-hold-gone", true);
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert!(rows[0].1.ends_with(" left submits=5"), "{rows:#?}");
    assert!(
        rows[1].0 == "skipped" && rows[1].1.ends_with(" why=gone"),
        "{rows:#?}"
    );
    let lives = end("left-hold-lives", false);
    assert!(
        lives.iter().any(|(_, r)| r.ends_with(" left submits=5"))
            && !lives.iter().any(|(d, _)| d == "skipped"),
        "CONTROL: {lives:#?}"
    );
}

/// THE DOUBLE TELL AND THE STRANDED LINE ARE THE ACCOUNT'S DEFECTS (negative controls of the
/// one settle): booked untold at its write (`ReleaseOnLeft`, every door before 2.9b), the line
/// the person sent is typed again past the pace, two lines for one episode; kept though its
/// composer was cleared (`LeftNeverSettles`), it is never typed again. The committed account
/// tells the sent line once and types the cleared one again.
#[test]
fn the_double_tell_and_the_stranded_line_are_the_accounts_defects() {
    let sent = Look {
        count: Some(6),
        ..AT_BREAK
    };
    let once = hold_left("left-hold-once", sent, &[60 * 60], None);
    assert_eq!(once.writes().len(), 2, "{:#?}", once.writes());
    let twice = hold_left(
        "left-hold-twice",
        Look {
            count: Some(6),
            ..AT_BREAK
        },
        &[60 * 60],
        Some(Fault::ReleaseOnLeft),
    );
    assert_eq!(
        twice.writes().len(),
        4,
        "NEGATIVE CONTROL: told again: {:#?}",
        twice.writes()
    );
    assert!(
        hold_rows(&twice)
            .first()
            .is_some_and(|(d, r)| d == "skipped" && r.ends_with(" why=not-submitted")),
        "{:#?}",
        twice.rows
    );
    let again = hold_left("left-hold-again", AT_BREAK, &[60 * 60], None);
    assert_eq!(again.writes().len(), 4, "{:#?}", again.writes());
    let stranded = hold_left(
        "left-hold-stranded",
        AT_BREAK,
        &[60 * 60],
        Some(Fault::LeftNeverSettles),
    );
    assert_eq!(
        stranded.writes().len(),
        2,
        "NEGATIVE CONTROL: never again: {:#?}",
        stranded.writes()
    );
    assert_eq!(hold_rows(&stranded).len(), 1, "{:#?}", stranded.rows);
}

/// A wake's line written at 5 minutes, its Enter refused, the window's book holding wake 1 ready;
/// then met as `look` says at 6 minutes.
fn wake_left(tag: &str, look: Look, gone: bool) -> (Ran, Arc<DoorHost>) {
    let host = real_host(real_book(WakeState::Ready));
    let line = job_line(1, T0 + MIN, None);
    let at_s: &[i64] = if gone { &[5 * 60] } else { &[5 * 60, 6 * 60] };
    let ran = Script {
        host: Arc::clone(&host),
        set: &enter_refused,
        gone,
        ..Script::new(tag, Occupancy::Prompt, at_s)
    }
    .run(&mut |s, i| {
        utc(s, i);
        if look.count.is_some() && i == 0 {
            submits(s, 5);
        }
        if i == 1 {
            if look.stands {
                standing(s, &line);
            }
            if let Some(n) = look.count {
                submits(s, n);
            }
            (look.then)(s);
        }
    });
    (ran, host)
}

/// The decisions and last words of the run's `fact-wake@v1` rows, oldest first.
fn wake_words(ran: &Ran) -> Vec<(String, String)> {
    wake_facts(ran)
        .into_iter()
        .map(|(decision, _, reason)| (decision, last_word(&reason).to_string()))
        .collect()
}

/// A WAKE'S LINE LEFT IN THE COMPOSER KEEPS ITS CLAIM UNTIL THE NEXT LOOK, AND A CLEARED COMPOSER
/// GIVES IT BACK: written and not submitted, the wake stays claimed in the window's book (no
/// second claim, no `await wake` answer) with no release, its `fact-wake@v1 waiting … left` row
/// booked; the next read settles it: the count risen, told through the break's door
/// (`wake_told`, the book's wake told); the composer clear and the count where it was, given back
/// (`wake_release`), so the book reads it ready and an `await wake` parked now is answered; the
/// line standing, still claimed; the session gone, given back.
#[test]
fn a_wakes_line_left_keeps_its_claim_until_the_look_and_a_cleared_composer_gives_it_back() {
    let (stands, host) = wake_left(
        "left-wake-stands",
        Look {
            stands: true,
            count: Some(6),
            ..AT_BREAK
        },
        false,
    );
    assert_eq!(
        wake_words(&stands),
        [("waiting".to_string(), "submits=5".to_string())],
        "{:#?}\n{:#?}",
        stands.requests(),
        stands.noted
    );
    assert_eq!(wake_door(&stands), ["wake_claim #1 break"]);
    let (state, answers) = real_after(&host);
    assert!(matches!(state, WakeState::Claimed { .. }), "{state:?}");
    assert!(answers.is_empty(), "no await answered while it stands");
    let line = job_line(1, T0 + MIN, None);
    let (sent, host) = wake_left(
        "left-wake-sent",
        Look {
            count: Some(6),
            ..AT_BREAK
        },
        false,
    );
    assert_eq!(
        wake_words(&sent),
        [
            ("waiting".to_string(), "submits=5".to_string()),
            ("typed".to_string(), "settled=submitted".to_string()),
        ]
    );
    assert_eq!(
        wake_door(&sent),
        [
            "wake_claim #1 break".to_string(),
            format!("inform {line}"),
            "wake_told #1 break".to_string(),
        ]
    );
    assert!(matches!(real_after(&host).0, WakeState::Told { .. }));
    let (cleared, host) = wake_left("left-wake-cleared", AT_BREAK, false);
    assert_eq!(
        wake_words(&cleared),
        [
            ("waiting".to_string(), "submits=5".to_string()),
            ("skipped".to_string(), "why=cleared".to_string()),
        ]
    );
    assert_eq!(
        wake_door(&cleared),
        ["wake_claim #1 break", "wake_release #1 break"]
    );
    assert_eq!(
        real_after(&host),
        (WakeState::Ready, vec![1]),
        "ready, and an await answers it"
    );
    let (gone, host) = wake_left("left-wake-gone", AT_BREAK, true);
    assert_eq!(
        wake_words(&gone),
        [
            ("waiting".to_string(), "submits=5".to_string()),
            ("skipped".to_string(), "why=gone".to_string()),
        ]
    );
    assert_eq!(real_after(&host), (WakeState::Ready, vec![1]));
    let (turn, host) = wake_left(
        "left-wake-turn",
        Look {
            count: None,
            stands: false,
            then: |s| {
                let screen = s.screen().expect("a live read");
                s.took(
                    screen.seq,
                    Some(&policy(Occupancy::Prompt)),
                    &mut Heard::default(),
                );
            },
        },
        false,
    );
    assert_eq!(
        wake_words(&turn),
        [
            ("waiting".to_string(), "left".to_string()),
            ("typed".to_string(), "settled=turn".to_string()),
        ]
    );
    assert!(matches!(real_after(&host).0, WakeState::Told { .. }));
}

/// A SUCCESSOR KEEPS ITS PREDECESSOR'S LEFT LINE AND SETTLES IT AT ITS OWN FIRST LOOK: the
/// claim a loop's door held with its line left stays claimed through the successor's reconcile
/// (no release, no tell at the seed) while the line stands. The successor cannot weigh its
/// predecessor's count (each host process counts its own submits from 0: a seamless update's
/// re-adopted session counts again), so its first read with the composer clear books the line
/// told (`typed … settled=uncounted`, the claim told), sent or cleared, and no door types it
/// again; nothing of the ledger's line is raised to the host (`inform`). NEGATIVE CONTROLS:
/// weighed (`ReleaseUncounted`), the same read gives the claim back (`why=cleared`) and the
/// door types the line again at once, a second tell where the person sent it; a reconcile that
/// gives back a claim whose line stands left (`ReleaseOnLeft`) releases it at the seed, the book
/// ready and an `await wake` answered while the line still stands in the composer.
#[test]
fn a_successor_settles_its_predecessors_left_line_at_its_first_look() {
    let (first, host) = wake_left(
        "left-wake-carry-first",
        Look {
            stands: true,
            count: Some(5),
            ..AT_BREAK
        },
        false,
    );
    let book = host.real.lock().expect("real").clone().expect("a book");
    assert!(
        matches!(
            book.get(1).map(|w| w.state),
            Some(WakeState::Claimed { .. })
        ),
        "{:#?}",
        first.rows
    );
    let line = job_line(1, T0 + MIN, None);
    let successor = |tag: &str, stands: bool, fault: Option<Fault>| {
        let host = real_host(book.clone());
        let run = || {
            Script {
                host: Arc::clone(&host),
                seed: Some((first.ledger.clone(), T0 + 7 * MIN)),
                ..Script::new(tag, Occupancy::Prompt, &[0])
            }
            .run(&mut |s, i| {
                utc(s, i);
                if stands {
                    standing(s, &line);
                }
                submits(s, 5);
            })
        };
        let ran = match fault {
            Some(f) => fault::with(f, run),
            None => run(),
        };
        (ran, host)
    };
    let (kept, host) = successor("left-wake-carry-kept", true, None);
    assert_eq!(wake_door(&kept), Vec::<String>::new(), "{:#?}", kept.calls);
    let (state, answers) = real_after(&host);
    assert!(matches!(state, WakeState::Claimed { .. }), "{state:?}");
    assert!(answers.is_empty(), "no await answered while it stands");
    let (told, host) = successor("left-wake-carry-cleared", false, None);
    assert_eq!(
        wake_door(&told),
        ["wake_told #1 break"],
        "told at the first look, never typed again: {:#?}",
        told.calls
    );
    let words = wake_words(&told);
    let settled = words
        .iter()
        .position(|(d, w)| d == "typed" && w == "settled=uncounted")
        .expect("told at the seed's look");
    assert!(words[settled + 1..].is_empty(), "{words:#?}");
    assert!(
        !told.calls.iter().any(|c| c.starts_with("inform ")),
        "the ledger's line is raised to no one: {:#?}",
        told.calls
    );
    assert!(matches!(real_after(&host).0, WakeState::Told { .. }));
    let (weighed, _) = successor(
        "left-wake-carry-weighed",
        false,
        Some(Fault::ReleaseUncounted),
    );
    let door = wake_door(&weighed);
    assert_eq!(
        door.first().map(String::as_str),
        Some("wake_release #1 break"),
        "NEGATIVE CONTROL: weighed, given back: {:#?}",
        weighed.calls
    );
    assert_eq!(
        door.iter().filter(|c| c.starts_with("wake_told ")).count(),
        1,
        "NEGATIVE CONTROL: typed again at once: {door:#?}"
    );
    let words = wake_words(&weighed);
    let cleared = words
        .iter()
        .position(|(d, w)| d == "skipped" && w == "why=cleared")
        .expect("released at the seed's look");
    assert!(
        words[cleared + 1..].len() == 1 && words[cleared + 1].0 == "typed",
        "{words:#?}"
    );
    let (early, host) = successor("left-wake-carry-early", true, Some(Fault::ReleaseOnLeft));
    assert_eq!(
        wake_door(&early),
        ["wake_release #1 break"],
        "NEGATIVE CONTROL: released at the seed: {:#?}",
        early.calls
    );
    assert_eq!(
        real_after(&host),
        (WakeState::Ready, vec![1]),
        "NEGATIVE CONTROL: an await answered while the line stands"
    );
}

/// THE WAKE'S LINE IS THE BREAK'S ONE LINE AND CRUISE'S CARRY NEVER TYPES OVER IT (the order
/// `Session::host_steps_in_background` keeps, pinned for 2.9b: the wake's door, the occupancy
/// design's rank 2, before the host's step, the stuck line and the nudges, and Cruise's carry
/// last, only where no line went): under Cruise and the owner's `prompt`, at a break where both
/// are due, the wake's line is typed and no `continue-background@v1` line; a wake's line written
/// and left in the composer is the break's one line too (`Wrote::Left`), no carry typed at that
/// break (the carry not even weighed there), and the carry is held at the next while the line
/// stands there. NEGATIVE CONTROLS: no wake declared, the same break carries; booked untold at
/// its write (`ReleaseOnLeft`, every door before 2.9b), the left line is no line and the break
/// goes on to weigh the carry, which only the composer's own guard keeps off the line.
#[test]
fn a_wakes_line_is_the_breaks_one_line_and_cruise_carries_nothing_over_it() {
    use crate::supervise::policy::cruise_carry::{BACKGROUND_CARRY, RULE_BACKGROUND};
    let line = job_line(1, T0 + MIN, None);
    let run_at = |tag: &str, wake: bool, left: bool, at_s: &[i64]| {
        let host = if wake {
            ready_host()
        } else {
            Arc::new(DoorHost::default())
        };
        let set = move |m: &mut Mock| {
            if left {
                enter_refused(m);
            }
        };
        Script {
            policy: SupervisorConfig {
                autopilot: Autopilot::Cruise,
                continue_policy: true,
                ..policy(Occupancy::Prompt)
            },
            host,
            set: &set,
            ..Script::new(tag, Occupancy::Prompt, at_s)
        }
        .run(&mut |s, i| {
            utc(s, i);
            if left && i == 1 {
                standing(s, &line);
            }
        })
    };
    let run = |tag: &str, wake: bool, left: bool| {
        let at_s: &[i64] = if left { &[5 * 60, 6 * 60] } else { &[5 * 60] };
        run_at(tag, wake, left, at_s)
    };
    let carries = |ran: &Ran| {
        ran.writes()
            .iter()
            .filter(|w| w.starts_with("send ") && w.ends_with(&format!(" -- {BACKGROUND_CARRY}")))
            .count()
    };
    // The carry weighed at a break: typed, or its hold or wait noted.
    let weighed = |ran: &Ran| {
        carries(ran)
            + ran
                .noted
                .iter()
                .filter(|l| l.starts_with("DEFERRED ") && l.contains(RULE_BACKGROUND))
                .count()
    };
    let woke = run("left-carry-wake", true, false);
    assert_eq!(woke.writes().len(), 2, "one line: {:#?}", woke.writes());
    assert!(
        woke.writes()[0].ends_with(&format!(" -- {line}")),
        "the wake's: {:#?}",
        woke.writes()
    );
    assert_eq!(carries(&woke), 0);
    let none = run("left-carry-none", false, false);
    assert_eq!(carries(&none), 1, "CONTROL: {:#?}", none.writes());
    let left = run("left-carry-left", true, true);
    assert_eq!(
        left.writes().len(),
        2,
        "the wake's line and its refused Enter alone: {:#?}",
        left.writes()
    );
    assert_eq!(carries(&left), 0);
    assert_eq!(
        wake_words(&left),
        [("waiting".to_string(), "left".to_string())]
    );
    let one = run_at("left-carry-one", true, true, &[5 * 60]);
    assert_eq!(weighed(&one), 0, "{:#?}", one.noted);
    let over = fault::with(Fault::ReleaseOnLeft, || {
        run_at("left-carry-over", true, true, &[5 * 60])
    });
    assert!(
        weighed(&over) > 0,
        "NEGATIVE CONTROL: the carry weighed past the line left: {:#?}",
        over.noted
    );
    assert_eq!(carries(&over), 0, "{:#?}", over.writes());
}

/// A wake's line left at a break by this loop, at `submits`.
fn left_wake(submits: Option<u64>) -> crate::supervise::facts::Left {
    crate::supervise::facts::Left {
        row: crate::supervise::occupancy::door::LeftRow::Wake {
            ord: 1,
            held_ms: 0,
            door: WakeDoor::Break,
        },
        text: "the line".to_string(),
        at_ms: T0,
        submits,
        ran: false,
        seeded: false,
        counted: true,
    }
}

/// The episode [`left_wake`]'s line is of.
const LEFT_KEY: crate::supervise::facts::EpisodeKey = crate::supervise::facts::EpisodeKey {
    kind: crate::supervise::facts::FactKind::Wake,
    id: 0x51,
};

/// A TURN SEEN UNDER A DIALOG IS KEPT FOR THE CLEAR READ AFTER IT (the 2.9b test review's F1):
/// on a host that serves no submit count, the live read after the left write finds a permission
/// box over the composer (unread) with the turn running, which is kept (`Left::ran`), and the
/// break's read after it with the composer clear tells the line (`settled=turn`), typed once.
/// NEGATIVE CONTROL: with no turn seen, the same clear read releases it and the rule types it
/// again past the pace.
#[test]
fn a_turn_seen_under_a_dialog_tells_the_left_line_at_the_clear_read_after() {
    let dialog = hold_left(
        "left-hold-dialog",
        Look {
            count: None,
            stands: false,
            then: |s| {
                s.ctl.m.screens = vec![bash_box(&["⏺ Pushing the branch."], "git push")];
                s.ctl.m.served = 0;
                let screen = s.screen().expect("a live read under a dialog");
                s.took(
                    screen.seq,
                    Some(&policy(Occupancy::Prompt)),
                    &mut Heard::default(),
                );
                s.ctl.m.screens = vec![break_screen()];
                s.ctl.m.served = 0;
            },
        },
        &[60 * 60],
        None,
    );
    let rows = hold_rows(&dialog);
    assert!(
        rows.len() == 2 && rows[1].0 == "typed" && rows[1].1.ends_with(" settled=turn"),
        "{rows:#?}"
    );
    assert_eq!(
        dialog.writes().len(),
        2,
        "told once: {:#?}",
        dialog.writes()
    );
    let unseen = hold_left(
        "left-hold-dialog-control",
        Look {
            count: None,
            ..AT_BREAK
        },
        &[60 * 60],
        None,
    );
    let rows = hold_rows(&unseen);
    assert!(
        rows.len() == 3 && rows[1].0 == "skipped" && rows[1].1.ends_with(" why=cleared"),
        "NEGATIVE CONTROL: {rows:#?}"
    );
    assert_eq!(unseen.writes().len(), 4, "{:#?}", unseen.writes());
}

/// A SUCCESSOR SEEDS NO LINE ITS PREDECESSOR ALREADY SETTLED (the 2.9b test review's F3): a wake's
/// line left and then given back at the predecessor's look (`why=cleared`, its episode's last
/// row) is not read back left (`FactsState::seed_left` keeps only an episode whose last row is its
/// left row), so the successor books no settle for it (no `settled=` or `why=` row) and its door
/// types the released wake as its own line. CONTROL: where the left row is the last
/// (`a_successor_settles_its_predecessors_left_line_at_its_first_look`), the successor settles it.
#[test]
fn a_successor_seeds_no_line_its_predecessor_settled() {
    let (first, host) = wake_left("left-wake-settled-first", AT_BREAK, false);
    let before = wake_words(&first);
    assert_eq!(
        before,
        [
            ("waiting".to_string(), "submits=5".to_string()),
            ("skipped".to_string(), "why=cleared".to_string()),
        ]
    );
    let book = host.real.lock().expect("real").clone().expect("a book");
    let next = Script {
        host: real_host(book),
        seed: Some((first.ledger.clone(), T0 + 7 * MIN)),
        ..Script::new("left-wake-settled-next", Occupancy::Prompt, &[0])
    }
    .run(&mut |s, i| {
        utc(s, i);
        submits(s, 5);
    });
    let words = wake_words(&next);
    let after = words.get(before.len()..).unwrap_or_default();
    assert!(
        !after.is_empty()
            && after
                .iter()
                .all(|(_, w)| !w.starts_with("settled=") && !w.starts_with("why=")),
        "{words:#?}"
    );
}

/// THE HOST BACK FROM AN OUTAGE WEIGHS NO COUNT FROM BEFORE IT (the 2.9b safety review's F2): the
/// loop's ride-out comes back to a host that may be another process, counting its submits from
/// 0, so a line left before it is booked told at a clear composer (`settled=uncounted`), sent or
/// cleared, never released for a retell. NEGATIVE CONTROL: with no outage, the same read weighs
/// the loop's own count and gives back the line cleared unsent.
#[test]
fn a_host_back_from_an_outage_weighs_no_count_from_before_it() {
    use crate::supervise::facts::{CLEARED, LeftLook, Settled, UNCOUNTED};
    let settle = |outage: bool| {
        let mut m = Mock::new(vec![break_screen()]);
        let mut s = virtual_session(&mut m, None);
        assert!(s.facts.state.leave(LEFT_KEY, left_wake(Some(5))));
        if outage {
            s.set_reconnect(Duration::from_millis(100));
            s.setup.pause = Duration::from_millis(10);
            let deadline = s.fx.mono() + Duration::from_secs(1);
            let back = s.ride_out("ERR the host went".to_string(), deadline, &mut |_| Ok(()));
            assert!(matches!(back, Ok(Rode::Back { .. })), "the host came back");
        }
        let read = LeftLook::Read {
            draft: Some(false),
            submits: Some(5),
            live: false,
        };
        s.facts
            .state
            .look(read, T0 + MIN)
            .into_iter()
            .map(|(_, _, how)| how)
            .collect::<Vec<_>>()
    };
    assert_eq!(settle(true), [Settled::Told(UNCOUNTED)]);
    assert_eq!(
        settle(false),
        [Settled::Released(CLEARED)],
        "NEGATIVE CONTROL: the loop's own count, weighed"
    );
}

/// A LOOK WITH NO LINE LEFT READS NOTHING, NOT EVEN THE CLOCK (the 2.9b test review's F6): the
/// policy's draft step's Enter and the session's end look at the account under every mode, and
/// with nothing left neither records a clock read a build before 2.9b did not make, so an older
/// tape's replay meets the records it holds. NEGATIVE CONTROL: with a line left, the draft step's
/// look reads the clock (its settle's stamp and its booking's) and the tape records it.
#[test]
fn a_look_with_no_line_left_reads_no_clock() {
    use crate::supervise::facts::LeftLook;
    use astream_effects::EffectRecord;
    let clocks = |left: bool| {
        let mut m = Mock::new(Vec::new());
        let (tape, mut taped) = memory_tape_on(&mut m, virtual_clock());
        {
            let mut s = taped_session(&mut taped, &tape);
            if left {
                assert!(s.facts.state.leave(LEFT_KEY, left_wake(Some(5))));
            }
            for look in [LeftLook::Submitted, LeftLook::Gone] {
                s.left_look(look, None, 0, None, None);
            }
        }
        drop(taped);
        tape.finish("looked")
            .items
            .iter()
            .filter(|i| matches!(i.record(), Some(EffectRecord::Clock(_))))
            .count()
    };
    assert_eq!(clocks(false), 0, "nothing left, no clock read");
    assert!(
        clocks(true) > 0,
        "NEGATIVE CONTROL: a settle's clock read is recorded"
    );
}

/// THE POINT DECIDED AGAIN IS A LOOK AT ITS NEW READ (`Session::turn_end_again`, the 2.9b test
/// review's F5): a line left with the count at 5, the point re-read with the composer clear,
/// settles there before the policy decides again: told where the re-read's count rose to 6,
/// given back where it stayed at 5. NEGATIVE CONTROL: the same point re-read with the line
/// standing settles nothing. Under `observe`, so the policy's draft step sends nothing.
#[test]
fn a_point_decided_again_looks_at_its_new_read() {
    let after = |again: Vec<String>, count: u64| {
        let mut m = Mock::new(vec![idle_screen(), again]);
        m.submits_on = [(0, 5)]
            .into_iter()
            .chain((1..64).map(|i| (i, count)))
            .collect();
        m.screen_cols.insert(1, 12);
        let opts = SuperviseOpts {
            policy: policy(Occupancy::Observe),
            ..SuperviseOpts::default()
        };
        let mut noted = Noted::default();
        let mut s = session(&mut m, None);
        assert!(s.facts.state.leave(LEFT_KEY, left_wake(Some(5))));
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        s.turn_end_now(&point, &opts, &[], &mut noted)
            .unwrap_or_else(|e| panic!("{e:?}"));
        (
            s.facts.state.is_left(LEFT_KEY),
            s.facts.state.is_told(LEFT_KEY),
        )
    };
    assert_eq!(after(idle_screen(), 6), (false, true), "sent: told");
    assert_eq!(
        after(idle_screen(), 5),
        (false, false),
        "cleared: given back"
    );
    assert_eq!(
        after(drafted(idle_screen(), "the line"), 6),
        (true, false),
        "NEGATIVE CONTROL: standing, nothing settles"
    );
}

/// A LINE TOLD AT A BREAK'S LOOK IS THE HARNESS'S TURN, AS A TYPED ONE IS (the 2.9b test review's
/// F5): a rule's line left at 31 minutes and sent by the person before the break read at 32 is
/// told there (`settled=submitted`); the agent's turn read live at 33 answers it, the harness's
/// turn (`TurnEndState::fact_typed`), and the point that acts at its end is the harness's too
/// (`HoldEvent::HarnessLine`), so the hold stays open through both. NEGATIVE CONTROLS:
/// `ToldLineUntaken` reads the answer as the agent's own turn and closes the hold
/// (`why=foreign`); `ToldLineUnanswered` closes it at the answer's point (`why=point`).
#[test]
fn a_line_told_at_a_breaks_look_is_the_harnesss_turn() {
    let closes = |tag: &str| {
        let shut = std::cell::RefCell::new(Vec::new());
        let script = Script {
            set: &enter_refused,
            ..Script::new(tag, Occupancy::Prompt, &[0, 31 * 60, 32 * 60, 33 * 60])
        };
        let ran = script.run(&mut |s, i| match i {
            0 => submits(s, 5),
            2 => submits(s, 6),
            3 => {
                let cfg = policy(Occupancy::Prompt);
                let mut heard = Heard::default();
                s.took(8, Some(&cfg), &mut heard);
                s.hold_point(&screen_at(8, break_screen()), true, Some(&cfg), &mut heard);
                shut.borrow_mut().extend(
                    heard
                        .noted
                        .into_iter()
                        .filter(|n| n.contains(" hold closed ")),
                );
            }
            _ => {}
        });
        let rows = hold_rows(&ran);
        assert!(
            rows.iter()
                .any(|(d, r)| d == "typed" && r.ends_with(" settled=submitted")),
            "{tag}: {rows:#?}"
        );
        shut.into_inner()
    };
    assert_eq!(closes("left-told-ours"), Vec::<String>::new());
    let untaken = fault::with(Fault::ToldLineUntaken, || closes("left-told-untaken"));
    assert!(
        untaken.len() == 1 && untaken[0].contains(" why=foreign "),
        "NEGATIVE CONTROL: {untaken:#?}"
    );
    let unanswered = fault::with(Fault::ToldLineUnanswered, || closes("left-told-unanswered"));
    assert!(
        unanswered.len() == 1 && unanswered[0].contains(" why=point "),
        "NEGATIVE CONTROL: {unanswered:#?}"
    );
}

/// A BOX'S READ SETTLES NOTHING (the 2.9b test review's F5): a composer drawn under a box is not
/// read (`Session::left_seen`), so a live read of a box with a composer drawn clear under it and
/// the count where it was gives nothing back, and the read after it, the line standing in the
/// composer, settles nothing either: the line stays left. NEGATIVE CONTROL: `BoxedComposerRead`
/// reads the composer under the box and gives back the line still standing (`why=cleared`).
#[test]
fn a_boxs_read_settles_nothing() {
    let run = |tag: &str| {
        hold_left(
            tag,
            Look {
                count: Some(5),
                stands: true,
                then: |s| {
                    let standing = std::mem::replace(
                        &mut s.ctl.m.screens,
                        vec![bash_box(&["⏺ Pushing the branch."], "git push")],
                    );
                    s.ctl.m.served = 0;
                    let screen = s.screen().expect("a live read of a box");
                    s.took(
                        screen.seq,
                        Some(&policy(Occupancy::Prompt)),
                        &mut Heard::default(),
                    );
                    s.ctl.m.screens = standing;
                    s.ctl.m.served = 0;
                },
            },
            &[],
            None,
        )
    };
    let rows = hold_rows(&run("left-hold-boxed"));
    assert!(
        rows.len() == 1 && rows[0].1.ends_with(" left submits=5"),
        "{rows:#?}"
    );
    let read = fault::with(Fault::BoxedComposerRead, || run("left-hold-boxed-read"));
    let rows = hold_rows(&read);
    assert!(
        rows.len() == 2 && rows[1].0 == "skipped" && rows[1].1.ends_with(" why=cleared"),
        "NEGATIVE CONTROL: {rows:#?}"
    );
}

/// A LINE LEFT STAYS READY-IDLE, HELD AS THE DRAFT IT STANDS AS (the 2.9b test review's F5, M25):
/// a rule's line held taskless from 31 minutes is raised ready-idle at its third look, and the
/// write at 35 that types it and finds its Enter refused leaves it in the composer: still due on
/// the K clock, its guard the draft it stands as (`Guard::Draft`, the same word its `OCCUPANCY`
/// line says), never a line the host's pace held. NEGATIVE CONTROL: the same write with its Enter
/// gone types it, and it is no longer due.
#[test]
fn a_line_left_stays_ready_idle_held_as_its_draft() {
    let run = |left: bool| {
        let host = Arc::new(DoorHost::default());
        host.taskless.store(true, Ordering::SeqCst);
        let free = Arc::clone(&host);
        let script = Script {
            host,
            set: if left { &enter_refused } else { &|_| {} },
            ..Script::new("left-k-draft", Occupancy::Prompt, &minutely(31, 35))
        };
        script.run(&mut |s, i| {
            if i == 0 {
                s.set_occupancy_k_looks(3);
            }
            if i == 5 {
                free.taskless.store(false, Ordering::SeqCst);
            }
        })
    };
    let ran = run(true);
    let rows = hold_rows(&ran);
    assert!(
        rows.len() == 1 && rows[0].0 == "waiting" && rows[0].1.ends_with(" left"),
        "{rows:#?}"
    );
    let last = ran.said.last().expect("told");
    assert_eq!(
        (last.at_ms, last.ready_idle),
        (
            T0 + 35 * MIN,
            Some(SaidReadyIdle {
                rule: RuleId::WaitNudge,
                guard: Some(Guard::Draft),
                since_ms: T0 + 31 * MIN,
            })
        ),
        "{:#?}",
        ran.said
    );
    let typed = run(false);
    let last = typed.said.last().expect("told");
    assert_eq!(
        (last.at_ms, last.ready_idle),
        (T0 + 35 * MIN, None),
        "NEGATIVE CONTROL: typed, no longer due"
    );
}
