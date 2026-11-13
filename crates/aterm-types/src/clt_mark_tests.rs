// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// A unix second well past any record's pause.
const NOW: u64 = 2_000_000_000;

/// [`Live`]'s files, with the processes a test says run.
struct World {
    running: std::cell::RefCell<Vec<(i32, u64)>>,
}

impl World {
    fn new() -> Self {
        Self {
            running: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn start(&self, asker: &Asker) {
        self.running.borrow_mut().push((asker.pid, asker.birth));
    }

    fn stop(&self, asker: &Asker) {
        self.running
            .borrow_mut()
            .retain(|running| *running != (asker.pid, asker.birth));
    }
}

impl Files for World {
    fn read(&self, path: &Path, max: usize) -> io::Result<Option<String>> {
        Live.read(path, max)
    }
    fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        Live.write(path, text)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        Live.remove(path)
    }
    fn lock(&self, path: &Path) -> io::Result<Held> {
        Live.lock(path)
    }
    fn runs(&self, pid: i32, birth: u64) -> bool {
        self.running.borrow().contains(&(pid, birth))
    }
}

fn asker(build: &str, pid: i32) -> Asker {
    Asker {
        build: build.to_owned(),
        pid,
        birth: 7,
    }
}

/// Every end an attempt can have, as the next automatic asker reads it: a cut-off attempt
/// (its asker gone with no end written: aterm quit or crashed, a reboot) and an
/// unfinished one are asked again by themselves, the cut-off at once and the unfinished after
/// [`ASK_AGAIN`]'s pause for its tries in a row; a finished one only after one pause; a
/// person's decline only after [`DECLINE_AGAIN`]'s long pause for their declines in a row
/// (the review found a decline was never asked again, a setup left undone for good), though
/// their own retry still claims at once.
/// NEGATIVE CONTROL: while the asker runs, nothing else claims; inside a decline's hold no
/// automatic ask claims.
#[test]
fn an_interrupted_or_unfinished_ask_comes_again_by_itself_and_a_decline_after_its_hold() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let window = asker("release", 501);
    let tab = asker("release", 502);
    assert!(reading(&world, &path, NOW).may_ask(NOW));

    let first = claim(&world, &path, &window, NOW)
        .unwrap()
        .expect("claimed");
    let first = hand(&world, &path, &first, tab.pid, tab.birth)
        .unwrap()
        .expect("handed to the tab's program");
    assert_eq!(first.asker, tab);
    world.start(&tab);
    assert!(matches!(reading(&world, &path, NOW), Asked::Asking));
    assert!(
        claim(&world, &path, &asker("update", 600), NOW)
            .unwrap()
            .is_none(),
        "CONTROL: no second asker beside a live one"
    );

    // The tab's program goes with no end written: cut off, due at once, no try added.
    world.stop(&tab);
    assert!(matches!(
        reading(&world, &path, NOW),
        Asked::Unfinished { due: NOW, tries: 0 }
    ));
    let again = claim(&world, &path, &asker("update", 600), NOW)
        .unwrap()
        .expect("an interrupted ask is asked again, by an update too");
    // That one ends unfinished (a password nobody typed): due after one pause.
    assert!(
        end(&world, &path, &again, End::Unfinished, NOW)
            .unwrap()
            .held()
    );
    let paused = ASK_AGAIN.after(1).as_secs();
    assert!(matches!(
        reading(&world, &path, NOW),
        Asked::Unfinished { tries: 1, due } if due == NOW + paused
    ));
    assert!(!reading(&world, &path, NOW + paused - 1).may_ask(NOW + paused - 1));
    assert!(
        claim(&world, &path, &window, NOW + paused - 1)
            .unwrap()
            .is_none()
    );
    let third = claim(&world, &path, &window, NOW + paused)
        .unwrap()
        .expect("due");
    assert!(
        end(&world, &path, &third, End::Unfinished, NOW + paused)
            .unwrap()
            .held()
    );
    assert!(matches!(
        reading(&world, &path, NOW + paused),
        Asked::Unfinished { tries: 2, due } if due == NOW + paused + ASK_AGAIN.after(2).as_secs()
    ));

    // A person declines: no automatic ask inside its long pause, though their own retry
    // claims at once; once it passes the ask comes again by itself, and a second no in a row
    // holds twice as long.
    let later = NOW + 100 * 24 * 3600;
    let fourth = claim(&world, &path, &window, later).unwrap().unwrap();
    assert!(
        end(&world, &path, &fourth, End::Declined, later)
            .unwrap()
            .held()
    );
    let hold = DECLINE_AGAIN.after(1).as_secs();
    assert!(matches!(
        reading(&world, &path, later),
        Asked::Declined { declines: 1, due } if due == later + hold
    ));
    assert!(!reading(&world, &path, later).coming());
    assert!(
        claim(&world, &path, &window, later + hold - 1)
            .unwrap()
            .is_none(),
        "CONTROL: inside its hold"
    );
    let again = later + hold;
    let fifth = claim(&world, &path, &window, again)
        .unwrap()
        .expect("its hold passed: asked again by itself");
    assert!(
        end(&world, &path, &fifth, End::Declined, again)
            .unwrap()
            .held()
    );
    assert!(matches!(
        reading(&world, &path, again),
        Asked::Declined { declines: 2, due } if due == again + DECLINE_AGAIN.after(2).as_secs()
    ));
    let person = explicit(&world, &path, &window, again).unwrap();
    assert!(
        end(&world, &path, &person, End::Done, again)
            .unwrap()
            .held()
    );
    assert!(matches!(
        reading(&world, &path, again),
        Asked::Done { due } if due == again + ASK_AGAIN.after(1).as_secs()
    ));
    assert!(reading(&world, &path, again).coming());
}

/// The record holds two attempts at once, a person's retry and the attempt it took over, and
/// each writes its own end in its own place, even where the retry is the same asker's in the
/// same second; a third attempt at once drops the oldest, whose late end then changes nothing.
/// An end of an attempt that asked nothing gives the record back exactly as it was before its
/// claim, whatever it was: no record, an unfinished try's schedule, a finish, a person's
/// decline, or another asker's live attempt.
/// NEGATIVE CONTROL: an unfinished end over each of those but the live attempt writes a try of
/// its own.
#[test]
fn only_the_holding_attempt_ends_it_and_a_release_restores_the_earlier_state() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let text = || std::fs::read_to_string(&path).unwrap();
    let window = asker("release", 501);
    let person = asker("release", 777);
    world.start(&window);
    world.start(&person);
    let older = claim(&world, &path, &window, NOW).unwrap().unwrap();
    let again = explicit(&world, &path, &window, NOW).unwrap();
    assert_ne!(
        again, older,
        "one asker's two claims in one second are two attempts"
    );
    // A third attempt at once: the oldest is given up, and its late end writes nothing.
    let theirs = explicit(&world, &path, &person, NOW + 1).unwrap();
    let before = text();
    for how in [End::Released, End::Unfinished, End::Declined] {
        assert_eq!(
            end(&world, &path, &older, how, NOW + 2).unwrap(),
            Took::Nowhere,
            "{how:?}"
        );
        assert_eq!(text(), before, "{how:?}: an end the record holds nowhere");
    }
    // The attempt the retry took over ends in its own place, under the retry, which asks on.
    assert!(
        end(&world, &path, &again, End::Unfinished, NOW + 2)
            .unwrap()
            .held()
    );
    assert!(matches!(reading(&world, &path, NOW + 2), Asked::Asking));
    assert!(
        end(&world, &path, &theirs, End::Released, NOW + 3)
            .unwrap()
            .held()
    );
    assert!(matches!(
        reading(&world, &path, NOW + 3),
        Asked::Unfinished { tries: 1, due } if due == NOW + 2 + ASK_AGAIN.after(1).as_secs()
    ));

    // Each earlier state, given back exactly by a release; an unfinished end writes a try.
    let follow_up = asker("update", 900);
    world.start(&follow_up);
    let live = format!("asking update 900 7 {NOW} 3 0 0 0\n");
    for earlier in [
        format!("unfinished release {} 1 0\n", NOW + 2),
        format!("done release {NOW}\n"),
        format!("declined release {NOW} {} 2\n", NOW - 50),
        live.clone(),
        "opened\n".to_owned(),
    ] {
        std::fs::write(&path, &earlier).unwrap();
        let pressed = explicit(&world, &path, &window, NOW + 3).unwrap();
        assert_ne!(text(), earlier, "{earlier:?}: claimed");
        assert!(
            end(&world, &path, &pressed, End::Released, NOW + 4)
                .unwrap()
                .held()
        );
        assert_eq!(text(), earlier, "a release gives back exactly {earlier:?}");
        if earlier == live {
            continue;
        }
        let pressed = explicit(&world, &path, &window, NOW + 5).unwrap();
        assert!(
            end(&world, &path, &pressed, End::Unfinished, NOW + 6)
                .unwrap()
                .held()
        );
        assert!(
            text().starts_with("unfinished release "),
            "CONTROL {earlier:?}: {}",
            text()
        );
    }
    // The follow-up's live attempt is still its own to end: given back by a release, and
    // asking on over a retry's end, so no automatic ask goes beside it (before, that end
    // replaced its line and an automatic claim went while it still asked).
    let theirs = Claim {
        asker: follow_up.clone(),
        at: NOW,
        n: 3,
    };
    for how in [End::Unfinished, End::Done, End::Declined] {
        std::fs::write(&path, &live).unwrap();
        let pressed = explicit(&world, &path, &window, NOW + 3).unwrap();
        assert!(
            end(&world, &path, &pressed, how, NOW + 4).unwrap().held(),
            "{how:?}"
        );
        assert!(
            text().starts_with("asking update 900 7 "),
            "{how:?}: {}",
            text()
        );
        assert!(matches!(reading(&world, &path, NOW + 4), Asked::Asking));
        assert!(
            claim(
                &world,
                &path,
                &asker("release", 600),
                NOW + 4 + 200 * 86_400
            )
            .unwrap()
            .is_none(),
            "{how:?}: no automatic ask beside the follow-up's"
        );
        assert!(
            end(&world, &path, &theirs, End::Done, NOW + 5)
                .unwrap()
                .held()
        );
        assert!(matches!(
            reading(&world, &path, NOW + 5),
            Asked::Done { .. }
        ));
    }
    // Once the follow-up is gone too, the retry's end is what the record says.
    std::fs::write(&path, &live).unwrap();
    let pressed = explicit(&world, &path, &window, NOW + 3).unwrap();
    world.stop(&follow_up);
    assert!(
        end(&world, &path, &pressed, End::Unfinished, NOW + 4)
            .unwrap()
            .held()
    );
    assert!(matches!(
        reading(&world, &path, NOW + 4),
        Asked::Unfinished { tries: 1, .. }
    ));

    std::fs::remove_file(&path).unwrap();
    let first = claim(&world, &path, &window, NOW).unwrap().unwrap();
    assert!(
        end(&world, &path, &first, End::Released, NOW)
            .unwrap()
            .held()
    );
    assert!(
        !path.exists(),
        "a first attempt that asked nothing leaves no record"
    );
}

/// Once setup's elevation finished, the record says so whatever comes after it (an
/// unfinished try, a decline, a live attempt): a step it did that reads undone later was
/// undone by someone. A person's yes to setup lifts their decline, back to that finish (or
/// to no record where none came before).
/// NEGATIVE CONTROL: a record that never finished says so, and a lift changes no other line.
#[test]
fn a_finish_outlives_later_tries_and_a_persons_yes_lifts_a_decline() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let window = asker("release", 501);
    assert!(!record(&world, &path, NOW).finished);
    let first = claim(&world, &path, &window, NOW).unwrap().unwrap();
    assert!(
        !record(&world, &path, NOW).finished,
        "CONTROL: asking, never finished"
    );
    assert!(
        end(&world, &path, &first, End::Unfinished, NOW)
            .unwrap()
            .held()
    );
    assert!(!record(&world, &path, NOW).finished, "CONTROL: unfinished");
    assert!(!lift(&world, &path).unwrap(), "CONTROL: no decline to lift");

    let later = NOW + 100_000;
    let second = claim(&world, &path, &window, later).unwrap().unwrap();
    assert!(
        end(&world, &path, &second, End::Done, later)
            .unwrap()
            .held()
    );
    assert!(record(&world, &path, NOW).finished);
    for (n, how) in [End::Unfinished, End::Declined].into_iter().enumerate() {
        let at = later + 100_000 * (n as u64 + 1);
        let next = explicit(&world, &path, &window, at).unwrap();
        assert!(record(&world, &path, NOW).finished, "{how:?}: while asking");
        assert!(end(&world, &path, &next, how, at).unwrap().held());
        assert!(record(&world, &path, NOW).finished, "{how:?}: after it");
    }
    let declined_at = later + 200_000;
    assert!(matches!(
        reading(&world, &path, declined_at),
        Asked::Declined { .. }
    ));
    assert!(lift(&world, &path).unwrap());
    assert!(
        matches!(reading(&world, &path, declined_at), Asked::Done { due } if due == later + ASK_AGAIN.after(1).as_secs()),
        "back to the finish"
    );
    assert!(record(&world, &path, NOW).finished);

    std::fs::remove_file(&path).unwrap();
    let never = claim(&world, &path, &window, NOW).unwrap().unwrap();
    assert!(
        end(&world, &path, &never, End::Declined, NOW)
            .unwrap()
            .held()
    );
    assert!(lift(&world, &path).unwrap());
    assert!(
        !path.exists(),
        "a decline that followed no finish lifts to no record"
    );
    assert!(reading(&world, &path, NOW).may_ask(NOW));
}

/// The attempt a person's retry took over (the window's tab, still asking) writes its own end
/// under the retry, which asks on, and the retry gives it back as the record's once it asked
/// nothing; a finish there is the account's whatever the retry then says (`CltEpisode`'s
/// `FinishOutlivesLaterTries`). Before, every such end matched nothing: a finish was lost, so
/// developer mode a person turned off later read as never set up and an automatic ask was owed
/// to turn it on again, and an unfinished try's schedule was lost with it.
/// NEGATIVE CONTROL: an end of that attempt that did not finish says nothing finished.
#[test]
fn a_finish_the_record_no_longer_held_is_kept() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let window = asker("release", 501);
    let person = asker("release", 777);
    world.start(&window);
    world.start(&person);
    for how in [End::Unfinished, End::Declined, End::Released] {
        std::fs::remove_file(&path).ok();
        let tab = claim(&world, &path, &window, NOW).unwrap().unwrap();
        let theirs = explicit(&world, &path, &person, NOW + 1).unwrap();
        assert_eq!(
            end(&world, &path, &tab, how, NOW + 2).unwrap(),
            Took::Replaced,
            "{how:?}"
        );
        assert!(
            matches!(reading(&world, &path, NOW + 2), Asked::Asking),
            "{how:?}: the retry asks on"
        );
        assert_eq!(
            end(&world, &path, &theirs, End::Released, NOW + 3).unwrap(),
            Took::Line
        );
        let read = reading(&world, &path, NOW + 3);
        match how {
            End::Unfinished => assert!(
                matches!(read, Asked::Unfinished { tries: 1, due } if due == NOW + 2 + ASK_AGAIN.after(1).as_secs()),
                "{read:?}"
            ),
            End::Declined => assert!(
                matches!(read, Asked::Declined { declines: 1, .. }),
                "{read:?}"
            ),
            _ => assert!(matches!(read, Asked::Available), "{read:?}"),
        }
        assert!(!record(&world, &path, NOW + 3).finished, "CONTROL {how:?}");
    }
    for how in [End::Unfinished, End::Declined, End::Released] {
        std::fs::remove_file(&path).ok();
        let tab = claim(&world, &path, &window, NOW).unwrap().unwrap();
        let theirs = explicit(&world, &path, &person, NOW + 1).unwrap();
        assert!(end(&world, &path, &tab, End::Done, NOW + 2).unwrap().held());
        assert!(
            record(&world, &path, NOW + 2).finished,
            "{how:?}: while theirs asks"
        );
        assert!(end(&world, &path, &theirs, how, NOW + 3).unwrap().held());
        assert!(
            record(&world, &path, NOW + 3).finished,
            "{how:?}: after theirs ended"
        );
    }
    // An attempt the record holds nowhere (a third at once dropped it, one deep), with no
    // line left at all: its finish is the line.
    std::fs::remove_file(&path).ok();
    let tab = claim(&world, &path, &window, NOW).unwrap().unwrap();
    let again = explicit(&world, &path, &window, NOW).unwrap();
    let theirs = explicit(&world, &path, &person, NOW + 1).unwrap();
    world.stop(&window);
    assert!(
        end(&world, &path, &again, End::Declined, NOW + 2)
            .unwrap()
            .held()
    );
    assert!(
        end(&world, &path, &theirs, End::Released, NOW + 2)
            .unwrap()
            .held()
    );
    assert!(lift(&world, &path).unwrap());
    assert!(!path.exists());
    assert_eq!(
        end(&world, &path, &tab, End::Done, NOW + 3).unwrap(),
        Took::Nowhere
    );
    assert!(matches!(
        reading(&world, &path, NOW + 3),
        Asked::Done { due } if due == NOW + 3 + ASK_AGAIN.after(1).as_secs()
    ));
}

/// An attempt that asked nothing gives the record back as it was before its claim, even where
/// a person's retry took the episode over and gave it back in between, which keeps the line
/// it replaced one deep (`CltEpisode`'s `TriesCountUnfinishedEnds` and
/// `FinishOutlivesLaterTries`): an unfinished try's schedule and the elevation's finish come
/// back with it. Before, that release removed the record, so the automatic ask went again at
/// once, and a finish was lost.
/// NEGATIVE CONTROL: a first claim's release, over no record, still leaves none.
#[test]
fn a_release_after_a_retry_gave_it_back_restores_the_earlier_state() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let text = || std::fs::read_to_string(&path).ok();
    let window = asker("release", 501);
    let person = asker("release", 777);
    world.start(&window);
    world.start(&person);
    for earlier in [
        format!("unfinished release {} 1 0\n", NOW - 10),
        format!("unfinished release {} 2 {}\n", NOW - 10, NOW - 50),
        format!("done release {}\n", NOW - 10),
    ] {
        std::fs::write(&path, &earlier).unwrap();
        let tab = explicit(&world, &path, &window, NOW).unwrap();
        let theirs = explicit(&world, &path, &person, NOW + 1).unwrap();
        assert!(
            end(&world, &path, &theirs, End::Released, NOW + 2)
                .unwrap()
                .held()
        );
        assert!(
            end(&world, &path, &tab, End::Released, NOW + 3)
                .unwrap()
                .held()
        );
        assert_eq!(text().as_deref(), Some(earlier.as_str()), "given back");
    }
    std::fs::remove_file(&path).unwrap();
    let tab = claim(&world, &path, &window, NOW).unwrap().unwrap();
    let theirs = explicit(&world, &path, &person, NOW + 1).unwrap();
    assert!(
        end(&world, &path, &theirs, End::Released, NOW + 2)
            .unwrap()
            .held()
    );
    assert!(
        end(&world, &path, &tab, End::Released, NOW + 3)
            .unwrap()
            .held()
    );
    assert_eq!(text(), None, "CONTROL: no record before, none after");
}

/// How the window records its tab's end, for every exit it can read: the program's own
/// status is its end, at a quit too (a finish just before the last window closed is still a
/// finish), and what aterm's quit hangs the program up with (a signal, no status, or the
/// stop's code, which the program exits with for a hangup as for a Ctrl-C) writes nothing,
/// so the record reads it cut off. Only a stop or a tab closed while aterm runs on declines.
/// NEGATIVE CONTROL: the same exits with aterm running on are each recorded.
#[test]
fn each_exit_of_the_tabs_program_decides_its_end_and_a_quits_hangup_writes_none() {
    use crate::macos_setup::{
        COMMAND_LINE_TOOLS_TAB_INSTALLER_HELD, COMMAND_LINE_TOOLS_TAB_LOW_DATA,
        COMMAND_LINE_TOOLS_TAB_NOT_ADMIN, COMMAND_LINE_TOOLS_TAB_NOT_OWN_LOGIN,
        COMMAND_LINE_TOOLS_TAB_NOT_STARTED, COMMAND_LINE_TOOLS_TAB_STOPPED,
        COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED,
    };
    let code = |code: u8| Some(TabExit::Code(i32::from(code)));
    for (exit, closed, running_on, at_quit) in [
        (
            Some(TabExit::Code(0)),
            false,
            Some(End::Done),
            Some(End::Done),
        ),
        (
            Some(TabExit::Code(1)),
            false,
            Some(End::Unfinished),
            Some(End::Unfinished),
        ),
        (
            code(COMMAND_LINE_TOOLS_TAB_STOPPED),
            false,
            Some(End::Declined),
            None,
        ),
        (
            code(COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED),
            false,
            Some(End::Unfinished),
            Some(End::Unfinished),
        ),
        (
            code(COMMAND_LINE_TOOLS_TAB_NOT_ADMIN),
            false,
            Some(End::Unfinished),
            Some(End::Unfinished),
        ),
        (
            code(COMMAND_LINE_TOOLS_TAB_LOW_DATA),
            false,
            Some(End::Released),
            Some(End::Released),
        ),
        (
            code(COMMAND_LINE_TOOLS_TAB_NOT_OWN_LOGIN),
            false,
            Some(End::Released),
            Some(End::Released),
        ),
        (
            code(COMMAND_LINE_TOOLS_TAB_INSTALLER_HELD),
            false,
            Some(End::Released),
            Some(End::Released),
        ),
        (
            code(COMMAND_LINE_TOOLS_TAB_NOT_STARTED),
            false,
            Some(End::Released),
            Some(End::Released),
        ),
        (
            Some(TabExit::Code(126)),
            false,
            Some(End::Released),
            Some(End::Released),
        ),
        (
            Some(TabExit::Code(127)),
            false,
            Some(End::Released),
            Some(End::Released),
        ),
        (
            Some(TabExit::Code(101)),
            false,
            Some(End::Unfinished),
            Some(End::Unfinished),
        ),
        (Some(TabExit::Signal), false, Some(End::Unfinished), None),
        (None, false, Some(End::Unfinished), None),
        (None, true, Some(End::Declined), None),
    ] {
        assert_eq!(
            tab_end(exit, closed, false),
            running_on,
            "{exit:?} closed={closed}"
        );
        assert_eq!(
            tab_end(exit, closed, true),
            at_quit,
            "{exit:?} closed={closed} at a quit"
        );
    }
}

/// Historical records (their end never written) read as one cut-off try, long since due;
/// malformed and unreadable ones hold every automatic ask and are left as found.
#[test]
fn historical_records_are_asked_again_and_unreadable_ones_hold() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let text = || std::fs::read_to_string(&path).unwrap();
    for historical in [
        "opened\n",
        "opened release\n",
        "opened 0.154.0+g319a3f1ca40a\nopened 0.160.0+gd7c37ab3c6df\n",
    ] {
        std::fs::write(&path, historical).unwrap();
        assert!(
            matches!(
                reading(&world, &path, NOW),
                Asked::Unfinished { tries: 1, .. }
            ),
            "{historical:?}"
        );
        assert!(reading(&world, &path, NOW).may_ask(NOW));
        let claimed = claim(&world, &path, &asker("update", 9), NOW).unwrap();
        assert!(claimed.is_some(), "{historical:?}");
    }

    let too_many = "opened again\n".repeat(LINES + 1);
    for malformed in [
        "",
        "\n",
        "opened two words\n",
        "closed\n",
        "asking release 1 2 3\n",
        "asking release 1 2 3 4 5 6 7 after\n",
        "asking release 1 2 3 4 5 6 7 after asking release 1 2 3 4 5 6 7 after done a 1\n",
        "unfinished release 1 2\n",
        "done release\n",
        "unfinished release 1 2\nunfinished release 1 2\n",
        "declined release 1 2 0\n",
        "declined release 1 2 3 4\n",
        too_many.as_str(),
    ] {
        std::fs::write(&path, malformed).unwrap();
        assert!(
            matches!(reading(&world, &path, NOW), Asked::Unread(ref error) if error.kind() == io::ErrorKind::InvalidData),
            "{malformed:?}"
        );
        assert!(!reading(&world, &path, NOW).coming());
        assert!(claim(&world, &path, &asker("release", 1), NOW).is_err());
        assert!(explicit(&world, &path, &asker("release", 1), NOW).is_err());
        assert_eq!(text(), malformed);
    }
    assert!(matches!(reading(&world, dir.path(), NOW), Asked::Unread(_)));
    let target = dir.path().join("elsewhere");
    std::fs::write(&target, "done dev 1\n").unwrap();
    let linked = dir.path().join("linked");
    std::os::unix::fs::symlink(&target, &linked).unwrap();
    assert!(matches!(reading(&world, &linked, NOW), Asked::Unread(_)));
    assert!(claim(&world, &linked, &asker("release", 1), NOW).is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "done dev 1\n");
    assert!(
        claim(
            &world,
            &dir.path().join("none"),
            &asker(&"x".repeat(BUILD_BYTES + 1), 1),
            NOW
        )
        .is_err()
    );
}

/// An earlier build's live tab, which an update handed to this build, is carried into a
/// historical record, or into none: the record then reads it asking while its program runs,
/// so no other automatic asker asks beside it, the end the adopting window writes against
/// the claim is recorded (a decline holds for its long pause), and with its program gone and
/// no end written it reads cut off. Carrying the same tab again gives back the same claim,
/// also from the line a person's retry over it keeps, which the retry gives back as it was.
/// NEGATIVE CONTROL: before the carry the historical record reads a try long since due; an
/// attempt of this format another asker made, or how one ended, is not carried over and is
/// left as found.
#[test]
fn an_earlier_builds_live_tab_is_carried_into_its_historical_record() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let tab = asker("0.193.0+g5a297ec3fd7c", 77);
    for historical in [
        Some("opened 0.193.0+g5a297ec3fd7c\n"),
        Some("opened\n"),
        None,
    ] {
        let _ = std::fs::remove_file(&path);
        if let Some(historical) = historical {
            std::fs::write(&path, historical).unwrap();
        }
        assert!(
            reading(&world, &path, NOW).may_ask(NOW),
            "{historical:?}: before the carry, an automatic ask may go"
        );
        world.start(&tab);
        let tabs_claim = carry(&world, &path, &tab, NOW).unwrap().expect("carried");
        assert_eq!(tabs_claim.asker, tab);
        assert!(
            matches!(reading(&world, &path, NOW), Asked::Asking),
            "{historical:?}"
        );
        assert!(
            claim(&world, &path, &asker("update", 9), NOW)
                .unwrap()
                .is_none(),
            "{historical:?}: no automatic ask beside it"
        );
        assert_eq!(
            carry(&world, &path, &tab, NOW).unwrap(),
            Some(tabs_claim.clone()),
            "{historical:?}: carried again, the same claim"
        );
        let person = asker("release", 79);
        world.start(&person);
        let retry = explicit(&world, &path, &person, NOW).unwrap();
        assert_eq!(
            carry(&world, &path, &tab, NOW).unwrap(),
            Some(tabs_claim.clone()),
            "{historical:?}: under a person's retry, the line it keeps still names the tab"
        );
        assert!(
            end(&world, &path, &retry, End::Released, NOW)
                .unwrap()
                .held()
        );
        world.stop(&person);
        assert!(
            matches!(reading(&world, &path, NOW), Asked::Asking),
            "{historical:?}: the retry gives the tab's line back"
        );
        world.stop(&tab);
        assert!(
            matches!(reading(&world, &path, NOW), Asked::Unfinished { .. }),
            "{historical:?}: gone with no end written, it was cut off"
        );
        assert!(
            end(&world, &path, &tabs_claim, End::Declined, NOW)
                .unwrap()
                .held()
        );
        assert!(
            matches!(
                reading(&world, &path, NOW),
                Asked::Declined { declines: 1, .. }
            ),
            "{historical:?}: its end is recorded"
        );
    }
    let other = asker("update", 78);
    world.start(&other);
    for kept in [
        format!("asking update 78 7 {NOW} 5 0 0 0\n"),
        format!("done release {NOW}\n"),
        format!("declined release {NOW} 0 1\n"),
        format!("unfinished release {NOW} 1 0\n"),
    ] {
        std::fs::write(&path, &kept).unwrap();
        assert_eq!(carry(&world, &path, &tab, NOW).unwrap(), None, "{kept:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), kept);
    }
    std::fs::write(&path, "closed\n").unwrap();
    assert!(carry(&world, &path, &tab, NOW).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "closed\n");
}

/// Each due is read at the reader's clock (review finding 10: the record used the plain
/// schedule, so a line dated after the clock, which was set back since, held the ask until
/// the clock came back past it, up to the schedule's longest pause and beyond): a try, a finish
/// or a decline dated after the clock is due at once, and the try then made records the
/// corrected time. A `declined` line written before the count of declines reads as the first.
/// NEGATIVE CONTROL: the same lines read at a clock past their dates wait their pauses.
#[test]
fn a_line_dated_after_the_clock_is_due_at_once_and_an_uncounted_decline_is_the_first() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let ahead = NOW + 3600;
    for line in [
        format!("unfinished release {ahead} 3 0\n"),
        format!("done release {ahead}\n"),
        format!("declined release {ahead} 0 1\n"),
        format!("asking release 1 7 {ahead} 3 2 0 0\n"),
    ] {
        std::fs::write(&path, &line).unwrap();
        assert!(
            reading(&world, &path, NOW)
                .due()
                .is_some_and(|due| due <= NOW),
            "{line:?}: due at once"
        );
        assert!(
            claim(&world, &path, &asker("release", 501), NOW)
                .unwrap()
                .is_some(),
            "{line:?}"
        );
        std::fs::write(&path, &line).unwrap();
        assert!(
            !reading(&world, &path, ahead).may_ask(ahead),
            "CONTROL {line:?}: read past its date, it waits its pause"
        );
    }
    std::fs::write(&path, format!("declined release {NOW} 0\n")).unwrap();
    assert!(matches!(
        reading(&world, &path, NOW),
        Asked::Declined { declines: 1, due } if due == NOW + DECLINE_AGAIN.after(1).as_secs()
    ));
    let person = explicit(&world, &path, &asker("release", 501), NOW).unwrap();
    assert!(
        end(&world, &path, &person, End::Declined, NOW)
            .unwrap()
            .held()
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("declined release {NOW} 0 2\n"),
        "the next no in a row counts on"
    );
}

/// A staged copy an earlier process of this pid left as it died mid-write (killed during the
/// flush, before its rename) holds no later change, and a link left at that name goes without
/// its target being touched.
/// NEGATIVE CONTROL: a writer that opens the staged name without clearing it first fails every
/// such change with `AlreadyExists`, the line it should write left as it was.
#[test]
fn a_staged_copy_a_dead_process_of_this_pid_left_holds_no_change() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let staged = dir
        .path()
        .join(format!("{FILE}.tmp-{}", std::process::id()));
    std::fs::write(&staged, "opened hal").unwrap();
    let first = claim(&world, &path, &asker("release", 5), NOW)
        .unwrap()
        .unwrap();
    assert!(!staged.exists(), "the staged copy went with the rename");

    std::fs::write(&staged, "opened hal").unwrap();
    assert!(
        end(&world, &path, &first, End::Unfinished, NOW)
            .unwrap()
            .held()
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("unfinished release {NOW} 1 0\n")
    );

    let target = dir.path().join("elsewhere");
    std::fs::write(&target, "kept\n").unwrap();
    std::os::unix::fs::symlink(&target, &staged).unwrap();
    assert!(explicit(&world, &path, &asker("release", 5), NOW).is_ok());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept\n");
    assert!(std::fs::symlink_metadata(&staged).is_err());
}

/// What `change` gives while another holder holds the record's lock at `path`, letting go
/// at the change's first wait on it where `lets_go` says so, else keeping it throughout; and
/// how many times the change found the lock held.
fn contended<T>(path: &Path, lets_go: bool, change: impl FnOnce() -> T) -> (T, usize) {
    let holder = std::rc::Rc::new(std::cell::RefCell::new(Some(Live.lock(path).unwrap())));
    let waits = std::rc::Rc::new(std::cell::Cell::new(0));
    let answer = aterm_uds::file_lock::with_lock_wait_observer(
        {
            let (holder, waits) = (holder.clone(), waits.clone());
            move |_| {
                waits.set(waits.get() + 1);
                if lets_go {
                    drop(holder.borrow_mut().take());
                }
            }
        },
        change,
    );
    drop(holder.borrow_mut().take());
    (answer, waits.get())
}

/// A change waits for the record's lock only while another asker holds it, and only within
/// its own wait from when it started. A terminal's change the scheduler started late still
/// waits out a holder that lets go within [`LOCK_WAIT`] of that start, so scheduling delay
/// alone does not fail the automatic ask's claim. A change on a window's loop gives up on a
/// holder that keeps the lock past [`LOOP_LOCK_WAIT`], and the record stays as it was. A
/// change whose wait ran out before it looked fails `TimedOut` without looking.
/// NEGATIVE CONTROL: each of those waits found the lock really held; with no holder the same
/// late start takes it at once, and an end of an attempt the record does not hold changes
/// nothing.
#[test]
fn a_scheduled_record_writer_does_not_spend_the_automatic_ask() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let late = std::time::Instant::now() - std::time::Duration::from_secs(3);
    let (taken, waits) = contended(&path, true, || lock_within(&path, late, LOCK_WAIT));
    assert!(
        taken.is_ok(),
        "a late change waits out its holder: {:?}",
        taken.as_ref().err()
    );
    assert!(waits >= 1, "it found the lock held");
    drop(taken);
    assert!(
        lock_within(&path, late, LOCK_WAIT).is_ok(),
        "with no holder it takes the lock at once"
    );
    assert!(matches!(
        lock_within(
            &path,
            std::time::Instant::now() - std::time::Duration::from_secs(61),
            LOCK_WAIT,
        ),
        Err(error) if error.kind() == io::ErrorKind::TimedOut
    ));

    let world = World::new();
    let owner = asker("owner", 501);
    let first = claim(&world, &path, &owner, NOW).unwrap().expect("claimed");
    world.start(&owner);
    let asking = std::fs::read_to_string(&path).unwrap();
    let (ended, waits) = contended(&path, false, || end(&OnLoop, &path, &first, End::Done, NOW));
    assert!(
        matches!(&ended, Err(error) if error.kind() == io::ErrorKind::WouldBlock),
        "the window's change gives up on a holder that keeps the lock: {ended:?}"
    );
    assert!(waits >= 1, "it found the lock held");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), asking);

    let later = asker("later", 502);
    assert!(claim(&world, &path, &later, NOW).unwrap().is_none());
    let stranger = Claim {
        asker: later.clone(),
        at: NOW,
        n: first.n + 1,
    };
    assert_eq!(
        end(&world, &path, &stranger, End::Released, NOW).unwrap(),
        Took::Nowhere
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), asking);
    assert!(
        end(&world, &path, &first, End::Released, NOW)
            .unwrap()
            .held()
    );
    assert!(claim(&world, &path, &later, NOW).unwrap().is_some());
}

/// A claim reads the record under its lock: a claim made while another asker holds the lock
/// waits for it, then finds the attempt that asker wrote and claims nothing, the record left
/// as that asker wrote it.
/// NEGATIVE CONTROL: with no other asker, the same claim takes the episode.
#[test]
fn of_two_builds_claiming_one_episode_only_one_asks() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let world = World::new();
    let other = asker("release", 41);
    world.start(&other);
    let held = std::rc::Rc::new(std::cell::RefCell::new(Some(Live.lock(&path).unwrap())));
    let waited = std::rc::Rc::new(std::cell::Cell::new(false));
    let line = format!("asking release 41 7 {NOW} 5 0 0 0\n");
    let claimed = aterm_uds::file_lock::with_lock_wait_observer(
        {
            let (held, waited, path, line) =
                (held.clone(), waited.clone(), path.clone(), line.clone());
            move |_| {
                if let Some(lock) = held.borrow_mut().take() {
                    // The other asker claims while it holds the lock, then lets go.
                    Live.write(&path, &line).unwrap();
                    drop(lock);
                    waited.set(true);
                }
            }
        },
        || claim(&world, &path, &asker("update", 42), NOW).unwrap(),
    );
    assert!(waited.get(), "the claim waited for the other asker's lock");
    assert!(claimed.is_none(), "the other asker claimed first");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), line);

    let alone = dir.path().join("alone");
    assert!(
        claim(&world, &alone, &asker("release", 1), NOW)
            .unwrap()
            .is_some()
    );
}

/// A first record makes the folder aterm's state lives in when there is none yet (a package
/// install before the window ever launched), and is written whole, readable by this account
/// alone, beside its lock file.
#[test]
fn a_first_record_makes_its_folder() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join("state").join("aterm").join(FILE);
    let world = World::new();
    assert!(
        claim(&world, &path, &asker("release", 3), NOW)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [FILE.to_owned(), format!("{FILE}.lock")],
        "no staged file left beside it"
    );
}

/// This process is an asker [`Live`] reads as running, and one of another birth is not.
#[test]
fn this_process_runs_and_a_recycled_pid_does_not() {
    let me = Asker::this_process("release").expect("this process's birth");
    assert!(Live.runs(me.pid, me.birth));
    assert!(!Live.runs(me.pid, me.birth.wrapping_add(1)));
    assert!(!Live.runs(1, me.birth));
}

/// The window (launchd gives it no `XDG_CONFIG_HOME`) and a terminal's `aterm init` (whose
/// shell may export one) find one record: its place is this account's aterm state, which no
/// `XDG_CONFIG_HOME` moves. This test binary runs itself twice over one state root (a lab
/// instance's `ATERM_STATE_HOME`; each test process has its own root otherwise), with the
/// variable set and with it unset, and both runs name the same file in that root.
/// NEGATIVE CONTROL: the config folder the run with the variable set reads is the variable's.
#[test]
fn one_record_whether_or_not_a_shell_exports_xdg_config_home() {
    const SAY: &str = "ATERM_TYPES_CLT_MARK_SAY_PATH";
    if std::env::var_os(SAY).is_some() {
        println!(
            "record={}",
            path().expect("a test binary's state root").display()
        );
        println!(
            "config={}",
            crate::dirs::aterm_config_dir()
                .expect("a test binary's config folder")
                .display()
        );
        return;
    }
    let dir = crate::test_scratch_dir("clt-mark");
    let state = dir.path().join("state");
    let xdg = dir.path().join("xdg");
    let test = format!(
        "{}::one_record_whether_or_not_a_shell_exports_xdg_config_home",
        module_path!().split_once("::").map_or("", |(_, rest)| rest)
    );
    let said = |xdg: Option<&Path>| {
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        child
            .args(["--exact", &test, "--nocapture", "--test-threads=1"])
            .env(SAY, "1")
            .env("ATERM_STATE_HOME", &state)
            .stdin(aterm_uds::spawnfd::Stdio::null());
        match xdg {
            Some(xdg) => child.env("XDG_CONFIG_HOME", xdg),
            None => child.env_remove("XDG_CONFIG_HOME"),
        };
        let out = child.output().expect("re-exec the test binary");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "the re-exec failed:\n{text}");
        let line = |key: &str| {
            // The harness writes the test's name ahead of its first line.
            text.lines()
                .find_map(|line| line.split_once(key).map(|(_, value)| value))
                .map(PathBuf::from)
                .unwrap_or_else(|| panic!("the re-exec named no {key}:\n{text}"))
        };
        (line("record="), line("config="))
    };
    let (set, config) = said(Some(&xdg));
    let (unset, _) = said(None);
    assert_eq!(set, state.join(FILE), "in the account's aterm state");
    assert_eq!(unset, set, "one file for both environments");
    assert_eq!(
        config,
        xdg.join("aterm"),
        "the config folder is the variable's"
    );
}
