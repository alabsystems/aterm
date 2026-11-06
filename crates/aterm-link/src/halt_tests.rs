// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The halt state machine's laws, each beside the negative control that breaks it.

use super::*;

const A: &str = "n-00000000000000aa";
const B: &str = "n-00000000000000bb";
/// The Mac applying what A and B said.
const ME: &str = "n-00000000000000cc";
const T0: u64 = 1_793_836_800_000;

fn id(tag: u8) -> String {
    format!("{tag:02x}").repeat(ID_HEX / 2)
}

fn on(tag: u8, by: &str, reason: &str) -> Said {
    Said {
        id: id(tag),
        by: by.to_string(),
        word: Word::On(reason.to_string()),
        t_ms: T0,
    }
}

fn off(tag: u8, by: &str) -> Said {
    Said {
        id: id(tag),
        by: by.to_string(),
        word: Word::Off,
        t_ms: T0,
    }
}

fn with<T>(
    cell: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
    f: impl FnOnce() -> T,
) -> T {
    cell.with(|c| c.set(true));
    let out = f();
    cell.with(|c| c.set(false));
    out
}

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}

/// **AN AGENT CANNOT HALT**: a record whose `by=` is an `a-` principal is no person's, so it is
/// refused before anything stands; a person's applies.
/// NEGATIVE CONTROL: with [`ANY_PRINCIPAL`] the agent's record is read as a person's and halts.
#[test]
fn an_agents_record_halts_nothing() {
    let agents = on(1, "a-review", "stop").render();
    assert_eq!(Said::parse(agents.as_bytes()), Err(Refused::NotAPerson));
    let persons = on(1, "h-andrew", "stop").render();
    assert_eq!(
        Said::parse(persons.as_bytes()),
        Ok(on(1, "h-andrew", "stop"))
    );

    let read = with(&ANY_PRINCIPAL, || Said::parse(agents.as_bytes()));
    let said = read.expect("control: the agent's record reads as a person's");
    let mut halts = Halts::default();
    assert_eq!(
        halts.apply(ME, A, &said, &none(), T0),
        Outcome::Halted,
        "control"
    );
}

/// **A REPLAYED HALT-OFF DOES NOT LIFT A LATER HALT**: a halt-off names the id it lifts, so the
/// same off coming again (a resend, a neighbour's copy, the log read anew) changes nothing for a
/// halt set after it.
/// NEGATIVE CONTROL: with [`OFF_LIFTS_EVERY`] a halt-off lifts whatever stands, as the old
/// per-person row did, and the replay lifts the later halt.
#[test]
fn a_replayed_halt_off_does_not_lift_a_later_halt() {
    for control in [false, true] {
        let case = format!("control={control}");
        let run = || {
            let mut halts = Halts::default();
            assert_eq!(
                halts.apply(ME, A, &on(1, "h-a", "one"), &none(), T0),
                Outcome::Halted
            );
            assert_eq!(
                halts.apply(ME, A, &off(1, "h-a"), &none(), T0 + 1),
                Outcome::Lifted
            );
            assert_eq!(
                halts.apply(ME, A, &on(2, "h-a", "two"), &none(), T0 + 2),
                Outcome::Halted
            );
            let replay = halts.apply(ME, A, &off(1, "h-a"), &none(), T0 + 3);
            (replay, halts.standing().contains_key(&id(2)))
        };
        let (replay, stands) = if control {
            with(&OFF_LIFTS_EVERY, run)
        } else {
            run()
        };
        if control {
            assert_eq!(
                replay,
                Outcome::Lifted,
                "{case}: the replay lifts the later halt"
            );
            assert!(!stands, "{case}");
        } else {
            assert_eq!(
                replay,
                Outcome::Already,
                "{case}: the replay changes nothing"
            );
            assert!(stands, "{case}: the later halt stands");
        }
    }
}

/// **A HALT-OFF FROM ANOTHER CURRENT MEMBER'S PERSON LIFTS A HALT**: a halt said on A by `h-a` is
/// lifted by a halt-off said on B by `h-b`.
/// NEGATIVE CONTROL: with [`OFF_FROM_ORIGIN_ONLY`] only A's `h-a` may lift it, and B's off is
/// refused.
#[test]
fn a_halt_off_from_another_members_person_lifts_it() {
    for control in [false, true] {
        let case = format!("control={control}");
        let run = || {
            let mut halts = Halts::default();
            halts.apply(ME, A, &on(1, "h-a", "stop"), &none(), T0);
            let answer = halts.apply(ME, B, &off(1, "h-b"), &none(), T0 + 1);
            (answer, halts.standing().is_empty())
        };
        let (answer, lifted) = if control {
            with(&OFF_FROM_ORIGIN_ONLY, run)
        } else {
            run()
        };
        if control {
            assert_eq!(answer, Outcome::Refused(Refused::NotAPerson), "{case}");
            assert!(!lifted, "{case}: the halt stands");
        } else {
            assert_eq!(answer, Outcome::Lifted, "{case}");
            assert!(lifted, "{case}: nothing stands");
        }
    }
}

/// **NO HALT OUTLIVES ITS ORIGIN**: once A is gone (removed, or its key rotated), its halts lift,
/// and nothing A said applies again; B's halt stands.
/// NEGATIVE CONTROL: with [`NO_LIFT_ON_GONE`] A's halt stands and its later record applies.
#[test]
fn a_gone_origins_halts_lift_and_what_it_said_is_refused() {
    for control in [false, true] {
        let case = format!("control={control}");
        let run = || {
            let mut halts = Halts::default();
            halts.apply(ME, A, &on(1, "h-a", "a"), &none(), T0);
            halts.apply(ME, B, &on(2, "h-b", "b"), &none(), T0);
            let gone: BTreeSet<String> = [A.to_string()].into();
            let lifted = halts.lift_gone(ME, &gone, T0 + 1);
            let later = halts.apply(ME, A, &on(3, "h-a", "again"), &gone, T0 + 2);
            (lifted, later, halts)
        };
        let (lifted, later, halts) = if control {
            with(&NO_LIFT_ON_GONE, run)
        } else {
            run()
        };
        assert!(
            halts.standing().contains_key(&id(2)),
            "{case}: B's halt stands"
        );
        if control {
            assert!(lifted.is_empty(), "{case}");
            assert_eq!(
                later,
                Outcome::Halted,
                "{case}: the gone origin halts again"
            );
            assert!(halts.standing().contains_key(&id(1)), "{case}");
        } else {
            assert_eq!(lifted, vec![id(1)], "{case}: A's halt lifted");
            assert_eq!(later, Outcome::Refused(Refused::OriginGone), "{case}");
            assert_eq!(halts.standing().len(), 1, "{case}");
        }
    }
}

/// A halt-off that comes before its halt (two paths, either order) keeps the id lifted, so the
/// halt never stands, however late it comes: a lifted id goes only once [`LIFTED_MAX`] later
/// lifts pushed it out, the oldest first.
#[test]
fn a_lifted_id_is_never_raised_again_in_any_order() {
    let mut halts = Halts::default();
    assert_eq!(
        halts.apply(ME, B, &off(1, "h-b"), &none(), T0),
        Outcome::Noted,
        "kept ahead of its halt"
    );
    assert_eq!(
        halts.apply(ME, A, &on(1, "h-a", "late"), &none(), T0 + 1),
        Outcome::Already
    );
    let years = 3 * 365 * 24 * 60 * 60 * 1000;
    assert_eq!(
        halts.apply(ME, A, &on(1, "h-a", "late"), &none(), T0 + years),
        Outcome::Already,
        "years later"
    );
    assert!(halts.standing().is_empty());
    assert!(halts.is_lifted(&id(1)));

    let nth = |n: usize| format!("{n:032x}");
    for n in 1..=LIFTED_MAX {
        halts.lift_here(&nth(n), "h-a", T0 + n as u64);
    }
    assert_eq!(halts.lifted().len(), LIFTED_MAX);
    assert!(!halts.knows(&id(1)), "the oldest went first");
    assert!(halts.is_lifted(&nth(1)) && halts.is_lifted(&nth(LIFTED_MAX)));
}

/// **A HALT-OFF FOR EVERY MAC AFTER A LIFT HERE IS STILL SAID AND KEPT**: a lift for this Mac
/// alone keeps the id lifted here and no more, so a later halt-off for every Mac is no repeat:
/// it makes the lift every Mac's, said by this Mac when its own person asked it.
#[test]
fn a_halt_off_after_a_lift_here_lifts_it_for_every_mac() {
    let mut halts = Halts::default();
    halts.apply(ME, A, &on(1, "h-a", "stop"), &none(), T0);
    assert_eq!(halts.lift_here(&id(1), "h-me", T0 + 1), Outcome::Lifted);
    assert_eq!(halts.lift_here(&id(1), "h-me", T0 + 2), Outcome::Already);
    assert!(halts.is_lifted(&id(1)) && !halts.lifted_everywhere(&id(1)));
    assert_eq!(
        halts.apply(ME, ME, &off(1, "h-me"), &none(), T0 + 3),
        Outcome::Noted
    );
    assert!(halts.lifted_everywhere(&id(1)));
    let lifted = &halts.lifted()[&id(1)];
    assert_eq!(
        (&lifted.how, lifted.at_ms),
        (
            &Lift::Fleet {
                by: "h-me".into(),
                said: true
            },
            T0 + 3
        )
    );
    assert_eq!(
        lifted.was.as_ref().map(|was| was.origin.as_str()),
        Some(A),
        "the halt it lifted is kept through both lifts"
    );
    assert_eq!(
        halts.apply(ME, B, &off(1, "h-b"), &none(), T0 + 4),
        Outcome::Already,
        "the first lift for every Mac stands"
    );
    assert_eq!(halts.lift_here(&id(1), "h-me", T0 + 5), Outcome::Already);
}

/// **A HALT SAID AGAIN AFTER A LIFT FOR EVERY MAC IS ANSWERED**: its origin says it again, so it
/// never heard the halt-off, and this Mac says the halt-off back, as the person who asked it.
/// The record said before the lift, this Mac's own, and one of a halt lifted here alone ask
/// nothing.
/// NEGATIVE CONTROL: the same halt said again after a lift for this Mac alone is not answered.
#[test]
fn a_halt_said_again_after_a_lift_for_every_mac_is_answered() {
    let again = |tag| Said {
        t_ms: T0 + 10,
        ..on(tag, "h-a", "stop")
    };
    let mut halts = Halts::default();
    halts.apply(ME, A, &on(1, "h-a", "stop"), &none(), T0);
    halts.apply(ME, B, &off(1, "h-b"), &none(), T0 + 1);
    assert_eq!(
        halts.apply(ME, A, &again(1), &none(), T0 + 11),
        Outcome::Answer { by: "h-b".into() }
    );
    assert_eq!(
        halts.apply(ME, A, &on(1, "h-a", "stop"), &none(), T0 + 12),
        Outcome::Already,
        "the record from before the lift"
    );
    assert_eq!(
        halts.apply(ME, ME, &again(1), &none(), T0 + 12),
        Outcome::Already,
        "this Mac's own"
    );
    assert!(halts.standing().is_empty());

    // Control: lifted here alone, the halt said again is no question for this Mac.
    halts.apply(ME, A, &on(2, "h-a", "stop"), &none(), T0);
    halts.lift_here(&id(2), "h-me", T0 + 1);
    assert_eq!(
        halts.apply(ME, A, &again(2), &none(), T0 + 11),
        Outcome::Already,
        "control: lifted here alone"
    );
    halts.apply(ME, A, &on(3, "h-a", "stop"), &none(), T0);
    halts.lift_gone(ME, &[A.to_string()].into(), T0 + 1);
    assert_eq!(
        halts.apply(ME, A, &again(3), &none(), T0 + 11),
        Outcome::Already,
        "lifted with its origin gone"
    );
}

/// The answer reads the origin's own clock: a record of the halt from before the lift, however
/// far the origin's clock runs ahead of this Mac's, asks nothing, and one said again while the
/// halt stood counts among them; only a stamp newer than every one before the lift is answered.
#[test]
fn a_halt_said_again_is_told_apart_on_its_origins_clock() {
    let at = |tag, t_ms| Said {
        t_ms,
        ..on(tag, "h-a", "stop")
    };
    let ahead = T0 + 10 * 60 * 1000;
    let mut halts = Halts::default();
    halts.apply(ME, A, &at(1, ahead), &none(), T0);
    assert_eq!(
        halts.apply(ME, A, &at(1, ahead + 5), &none(), T0 + 1),
        Outcome::Noted,
        "said again while it stands"
    );
    halts.apply(ME, B, &off(1, "h-b"), &none(), T0 + 2);
    for t_ms in [ahead, ahead + 5] {
        assert_eq!(
            halts.apply(ME, A, &at(1, t_ms), &none(), T0 + 3),
            Outcome::Already,
            "a record from before the lift, stamped {t_ms}"
        );
    }
    assert_eq!(
        halts.apply(ME, A, &at(1, ahead + 6), &none(), T0 + 4),
        Outcome::Answer { by: "h-b".into() }
    );
}

/// **A MAC THE LOG TOOK OUT LIFTS ITS OWN HALTS, ONCE**: the halts it said lift when the
/// hand-off first names it gone, as on every other Mac; what its person asks after that is its
/// own and stops its own sessions, through a restart and a later change of who is gone. Another
/// gone node's records stay refused.
/// NEGATIVE CONTROL: without the kept mark (`out`), the next pass over the gone nodes lifts the
/// halt asked after the removal too.
#[test]
fn a_mac_the_log_took_out_lifts_its_own_halts_once() {
    let mut halts = Halts::default();
    halts.apply(ME, ME, &on(1, "h-me", "before"), &none(), T0);
    halts.apply(ME, A, &on(2, "h-a", "theirs"), &none(), T0);
    let out: BTreeSet<String> = [ME.to_string()].into();
    assert_eq!(halts.lift_gone(ME, &out, T0 + 1), vec![id(1)]);
    assert!(halts.standing().contains_key(&id(2)), "A's halt stands");
    assert_eq!(
        halts.apply(ME, ME, &on(3, "h-me", "after"), &out, T0 + 2),
        Outcome::Halted,
        "its person still stops it"
    );
    let more: BTreeSet<String> = [ME.to_string(), B.to_string()].into();
    assert_eq!(halts.lift_gone(ME, &more, T0 + 3), Vec::<String>::new());
    assert_eq!(
        halts.apply(ME, B, &on(4, "h-b", "gone"), &more, T0 + 4),
        Outcome::Refused(Refused::OriginGone)
    );
    let (mut back, damaged) = Halts::parse(&halts.render()).expect("parses");
    assert!(damaged.is_empty());
    assert_eq!(back, halts);
    assert_eq!(
        back.lift_gone(ME, &out, T0 + 5),
        Vec::<String>::new(),
        "after a restart"
    );
    assert!(back.standing().contains_key(&id(3)));

    let mut control = halts.clone();
    control.out = false;
    assert_eq!(
        control.lift_gone(ME, &out, T0 + 5),
        vec![id(3)],
        "control: the halt asked after the removal lifts"
    );
}

/// **THE ORIGIN SAYS ITS OWN HALTS AGAIN, LIFTED OR NOT**: a halt this Mac said that its person
/// lifted here alone still stands on the other Macs, so it is said again as the halt; one a
/// halt-off from another Mac lifted for every Mac has its halt-off said again within the span,
/// whoever asked it. Another Mac's halt lifted here alone is not this Mac's to say.
#[test]
fn the_origin_says_again_its_halts_lifted_here_and_the_halt_offs_of_its_halts() {
    let mut halts = Halts::default();
    halts.apply(ME, ME, &on(1, "h-me", "mine"), &none(), T0);
    halts.lift_here(&id(1), "h-me", T0);
    halts.apply(ME, ME, &on(2, "h-me", "mine"), &none(), T0);
    halts.apply(ME, B, &off(2, "h-b"), &none(), T0);
    halts.apply(ME, A, &on(3, "h-a", "theirs"), &none(), T0);
    halts.lift_here(&id(3), "h-me", T0);
    let due = |now| -> Vec<(String, Word, String)> {
        halts
            .due(ME, &BTreeMap::new(), now)
            .into_iter()
            .map(|said| (said.id, said.word, said.by))
            .collect()
    };
    assert_eq!(
        due(T0),
        vec![
            (id(1), Word::On("mine".into()), "h-me".into()),
            (id(2), Word::Off, "h-b".into()),
        ]
    );
    assert_eq!(
        due(T0 + OFF_RESAY_SPAN_MS),
        vec![(id(1), Word::On("mine".into()), "h-me".into())],
        "the halt for as long as it stands elsewhere, the halt-off within the span"
    );
}

/// **WHAT A MAC SAYS AGAIN**, on an injected clock: each of its own standing halts and each
/// halt-off its own person asked, once [`RESAY_MS`] passed since it said it last (at once when
/// no log still holds it), never before; a halt-off only within [`OFF_RESAY_SPAN_MS`] of the
/// lift; never another Mac's halt, a halt-off heard (one that lifted nothing here, or another
/// Mac's halt that stood here), or a lift here.
#[test]
fn a_mac_says_its_records_again_after_resay_ms_and_its_offs_within_the_span() {
    let mut halts = Halts::default();
    halts.apply(ME, ME, &on(1, "h-me", "mine"), &none(), T0);
    halts.apply(ME, A, &on(2, "h-a", "theirs"), &none(), T0);
    halts.apply(ME, ME, &off(3, "h-me"), &none(), T0);
    halts.apply(ME, B, &off(4, "h-b"), &none(), T0);
    halts.lift_here(&id(5), "h-me", T0);
    // A's halt, which stood here, lifted by B's person: the halt-off is heard, and neither this
    // Mac's person's nor of a halt of this Mac's.
    halts.apply(ME, A, &on(6, "h-a", "theirs"), &none(), T0);
    halts.apply(ME, B, &off(6, "h-b"), &none(), T0);
    assert!(halts.is_lifted(&id(6)));
    let ids = |due: Vec<Said>| -> Vec<(String, bool)> {
        due.into_iter()
            .filter(|said| said.word != Word::Ask)
            .map(|said| (said.id, said.word == Word::Off))
            .collect()
    };
    let said_at: BTreeMap<String, u64> = [(id(1), T0), (id(3), T0)].into();
    assert_eq!(ids(halts.due(ME, &said_at, T0 + RESAY_MS - 1)), vec![]);
    let due = halts.due(ME, &said_at, T0 + RESAY_MS);
    assert!(due.iter().all(|said| said.t_ms == T0 + RESAY_MS));
    assert_eq!(ids(due), vec![(id(1), false), (id(3), true)]);
    assert_eq!(
        ids(halts.due(ME, &BTreeMap::new(), T0)),
        vec![(id(1), false), (id(3), true)],
        "at once when no log holds them"
    );
    assert_eq!(
        ids(halts.due(ME, &said_at, T0 + OFF_RESAY_SPAN_MS)),
        vec![(id(1), false)],
        "a standing halt for as long as it stands, a halt-off within the span"
    );
}

fn ask(tag: u8, by: &str, t_ms: u64) -> Said {
    Said {
        id: id(tag),
        by: by.to_string(),
        word: Word::Ask,
        t_ms,
    }
}

/// **A HALT HELD PAST ITS ORIGIN'S SAYING IS ASKED ABOUT, AND A MAC THAT LIFTED IT ANSWERS** (the
/// case C4 left open, judged by the `FabricHalts` model): A halted, B's person lifted it for every
/// Mac, and this Mac was away past [`OFF_RESAY_SPAN_MS`], so no log holds the halt-off and no Mac
/// says it any more. Once A's newest saying is [`STALE_MS`] old this Mac asks, every
/// [`RESAY_MS`]; B, which lifted it for every Mac, answers with the halt-off, which lifts it here.
/// An ask for a halt the hearer did not lift for every Mac (one standing there, lifted there
/// alone, or never heard of) is answered by nothing and keeps nothing; this Mac never asks for its
/// own halts, for a halt A said within [`STALE_MS`], or once the log took it out.
/// NEGATIVE CONTROL: [`NO_STALE_ASK`] (the path before): this Mac never asks, and holds the halt
/// every other Mac lifted.
#[test]
fn a_stale_halt_is_asked_about_and_a_mac_that_lifted_it_answers() {
    for control in [false, true] {
        let case = format!("control={control}");
        let mut here = Halts::default();
        let mut b = Halts::default();
        let a_halt = on(1, "h-a", "stop");
        here.apply(ME, A, &a_halt, &none(), T0);
        b.apply(B, A, &a_halt, &none(), T0);
        here.apply(ME, ME, &on(2, "h-me", "mine"), &none(), T0);
        b.apply(B, B, &off(1, "h-b"), &none(), T0 + 1);
        let late = T0 + OFF_RESAY_SPAN_MS + DAY_MS;
        assert!(
            b.due(B, &BTreeMap::new(), late).is_empty(),
            "{case}: B says nothing past the span"
        );
        let asks = |halts: &Halts, said_at: &BTreeMap<String, u64>, now: u64| -> Vec<String> {
            halts
                .due(ME, said_at, now)
                .into_iter()
                .filter(|said| said.word == Word::Ask)
                .map(|said| said.id)
                .collect()
        };
        NO_STALE_ASK.with(|c| c.set(control));
        assert!(
            asks(&here, &BTreeMap::new(), T0 + STALE_MS - 1).is_empty(),
            "{case}: fresh"
        );
        let asked = asks(&here, &BTreeMap::new(), late);
        let again = asks(&here, &[(id(1), late)].into(), late + RESAY_MS - 1);
        let after = asks(&here, &[(id(1), late)].into(), late + RESAY_MS);
        NO_STALE_ASK.with(|c| c.set(false));
        if control {
            assert!(asked.is_empty() && after.is_empty(), "{case}: never asks");
            assert!(
                here.standing().contains_key(&id(1)),
                "{case}: holds what every Mac lifted"
            );
            continue;
        }
        assert_eq!(
            asked,
            vec![id(1)],
            "{case}: asks for A's halt, never its own"
        );
        assert!(again.is_empty(), "{case}: not again within RESAY_MS");
        assert_eq!(after, vec![id(1)], "{case}: again after RESAY_MS");
        let question = ask(1, "h-a", late);
        assert_eq!(
            Said::parse(question.render().as_bytes()),
            Ok(question.clone()),
            "{case}: an ask reads back"
        );
        assert_eq!(
            Ask::parse(question.render().as_bytes()),
            None,
            "{case}: no person's request asks"
        );
        assert_eq!(
            b.apply(B, ME, &question, &none(), late),
            Outcome::Answer { by: "h-b".into() },
            "{case}: B lifted it for every Mac, so B answers"
        );
        // Nothing else answers, and nothing keeps the question.
        let mut standing = Halts::default();
        standing.apply(B, A, &a_halt, &none(), T0);
        let mut alone = standing.clone();
        alone.lift_here(&id(1), "h-b", T0 + 1);
        for (what, mut hearer) in [
            ("standing there", standing),
            ("lifted there alone", alone),
            ("never heard of", Halts::default()),
        ] {
            let before = hearer.clone();
            assert_eq!(
                hearer.apply(B, ME, &question, &none(), late),
                Outcome::Already,
                "{case}: a halt {what} is not answered"
            );
            assert_eq!(
                hearer, before,
                "{case}: an ask for a halt {what} keeps nothing"
            );
        }
        let answer = Said {
            t_ms: late,
            ..off(1, "h-b")
        };
        assert_eq!(here.apply(ME, B, &answer, &none(), late), Outcome::Lifted);
        assert!(
            !here.standing().contains_key(&id(1)),
            "{case}: the answer lifts it here"
        );
        let mut out = Halts::default();
        out.apply(ME, A, &a_halt, &none(), T0);
        out.lift_gone(ME, &[ME.to_string()].into(), T0);
        assert!(
            asks(&out, &BTreeMap::new(), late).is_empty(),
            "{case}: a Mac the log took out asks nothing"
        );
    }
}

/// The sessions are held under the newest standing halt's reason; none once nothing stands.
#[test]
fn the_newest_standing_halt_names_the_reason() {
    let mut halts = Halts::default();
    assert_eq!(halts.reason(), None);
    halts.apply(ME, A, &on(1, "h-a", "first"), &none(), T0);
    halts.apply(ME, B, &on(2, "h-b", "second"), &none(), T0 + 5);
    assert_eq!(halts.reason(), Some("second"));
    halts.apply(ME, A, &off(2, "h-a"), &none(), T0 + 6);
    assert_eq!(halts.reason(), Some("first"));
}

/// The state file keeps every standing halt, every lifted id and how, and the move's mark; a damaged line
/// is named and skipped, never a reason to forget the rest, and a lift beats a stand.
#[test]
fn the_state_file_round_trips_and_a_damaged_line_costs_only_itself() {
    let mut halts = Halts::default();
    halts.apply(ME, A, &on(1, "h-a", "main%20is%20broken"), &none(), T0);
    halts.apply(ME, A, &on(2, "h-a", "two"), &none(), T0);
    halts.apply(ME, B, &off(2, "h-b"), &none(), T0 + 1);
    halts.apply(ME, ME, &off(3, "h-me"), &none(), T0 + 1);
    halts.lift_here(&id(4), "h-me", T0 + 2);
    halts.apply(ME, B, &on(6, "h-b", "six"), &none(), T0);
    halts.lift_gone(ME, &[B.to_string()].into(), T0 + 3);
    halts.set_moved();
    let text = halts.render();
    let (back, damaged) = Halts::parse(&text).expect("parses");
    assert!(damaged.is_empty());
    assert_eq!(back, halts);

    let torn = format!(
        "{text}on {} {A} h-a notanumber 1 r\noff zz 1 gone\noff {} 1\nlater kind\non {} {A} h-a \
         5 5 r\noff {} 9 gone\n",
        id(3),
        id(5),
        id(4),
        id(4)
    );
    let (back, damaged) = Halts::parse(&torn).expect("parses");
    assert_eq!(damaged.len(), 3, "{damaged:?}");
    assert!(back.moved());
    assert!(back.standing().contains_key(&id(1)));
    assert!(
        !back.standing().contains_key(&id(4)),
        "lifted beats standing"
    );
    assert!(back.is_lifted(&id(4)));
    assert!(Halts::parse("mesh-halts v9\n").is_err());
}

/// A request and a record read back as written; one with no id, a malformed id, no state or no
/// `by=` reads as nothing.
#[test]
fn requests_and_records_read_back_and_malformed_ones_are_nothing() {
    let ask = Ask {
        id: id(7),
        word: Word::On(reason_token(Some("main is broken"))),
        here: false,
    };
    assert_eq!(Ask::parse(ask.render(T0).as_bytes()), Some(ask.clone()));
    let lift = Ask {
        id: id(7),
        word: Word::Off,
        here: false,
    };
    assert_eq!(Ask::parse(lift.render(T0).as_bytes()), Some(lift.clone()));
    let here = Ask { here: true, ..lift };
    assert_eq!(Ask::parse(here.render(T0).as_bytes()), Some(here));
    assert_eq!(
        Ask::parse(format!("v=1 t=1 state=on id={} reason=x scope=here", id(7)).as_bytes())
            .map(|ask| ask.here),
        Some(false),
        "a halt is never for one Mac"
    );
    assert_eq!(Ask::parse(b"v=1 t=1 state=on reason=x"), None, "no id");
    assert_eq!(
        Ask::parse(b"v=1 t=1 state=on id=XY reason=x"),
        None,
        "not an id"
    );
    assert_eq!(
        Ask::parse(format!("v=1 t=1 id={}", id(7)).as_bytes()),
        None,
        "no state"
    );
    let no_by = format!("v=1 t=1 state=off id={}", id(7));
    assert_eq!(Said::parse(no_by.as_bytes()), Err(Refused::Malformed));
    assert_eq!(reason_token(None), REASON_DEFAULT);
    assert_eq!(
        reason_token(Some("x".repeat(400).as_str())).len(),
        REASON_MAX
    );
}

/// A carried halt's id is the same on every run and distinct per Mac, fleet and person, and a
/// carried halt that was lifted is not carried again.
#[test]
fn a_carried_id_is_stable_and_distinct() {
    let one = carried_id(A, "local", "h-a");
    assert!(is_id(&one));
    assert_eq!(one, carried_id(A, "local", "h-a"));
    for other in [
        carried_id(B, "local", "h-a"),
        carried_id(A, "f2", "h-a"),
        carried_id(A, "local", "h-b"),
        carried_id("n-00000000000000a", "alocal", "h-a"),
    ] {
        assert_ne!(one, other);
    }
    let mut halts = Halts::default();
    assert!(halts.carry(&one, A, "h-a", "stop", T0));
    assert!(!halts.carry(&one, A, "h-a", "stop", T0), "carried once");
    halts.apply(
        ME,
        B,
        &Said {
            id: one.clone(),
            by: "h-b".into(),
            word: Word::Off,
            t_ms: T0,
        },
        &none(),
        T0,
    );
    assert!(
        !halts.carry(&one, A, "h-a", "stop", T0),
        "a lifted carry is not raised"
    );
}

/// The subjects: a request names a person (never an agent), and a halt subject names a node.
#[test]
fn the_halt_subjects_name_a_person_and_a_node() {
    use crate::subject::{halt_ask, halt_subject, parse_halt_ask, parse_halt_origin};
    assert_eq!(
        parse_halt_ask("alab", &halt_ask("alab", "h-andrew")),
        Some("h-andrew")
    );
    assert_eq!(parse_halt_ask("alab", &halt_ask("alab", "a-review")), None);
    assert_eq!(parse_halt_ask("alab", &halt_ask("other", "h-andrew")), None);
    assert_eq!(
        parse_halt_ask("alab", "/f/alab/fleet/h-andrew/halt"),
        None,
        "the old row"
    );
    assert_eq!(parse_halt_origin("alab", &halt_subject("alab", A)), Some(A));
    assert_eq!(
        parse_halt_origin("alab", &format!("/f/alab/halt/{A}/deeper")),
        None
    );
    assert_eq!(parse_halt_origin("alab", "/f/alab/halt/h-andrew"), None);
}
