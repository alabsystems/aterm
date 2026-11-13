// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Why the seat rule bars this process's session `sid` on volume `key` now ([`judge_at`]).
/// `None`: it may be asked, or it is not this process's.
fn bar_at(store: &Store, sid: &str, key: &str) -> Option<&'static str> {
    match judge_at(store, sid, key, &Drafts::default()) {
        Judged::Barred(why) => Some(why),
        Judged::Seat | Judged::Unseated | Judged::Held(_) => None,
    }
}

fn rows(state: &Path) -> Vec<String> {
    std::fs::read_to_string(state.join(aterm_agent::harness::cli::DISK_LEDGER))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A DISK WORKER'S RECORD READS BY HOW IT ENDED: a landing an exit followed finished, an agent
/// that left first or a worker that said it could not finish failed with its word, and a stall
/// is no end, so it reads working. A failed session bars a seat as a finished one does.
#[test]
fn a_workers_record_reads_by_how_it_ended() {
    use crate::task_record::{
        TaskMark, WHY_COULD_NOT_FINISH, WHY_EXIT, WHY_LANDED, mark_session_task,
    };
    let mut sessions = crate::session_store::SessionStore::default();
    let h = crate::session_store::test_handle(9_301);
    sessions.register(h.clone());
    let sid = h.sid.as_str().to_owned();
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
    let mark = |state, why| {
        let mark = TaskMark {
            why,
            ..TaskMark::new(state)
        };
        mark_session_task(&h.ctx, 9_301, &mark).expect("unfenced");
        task_seen(&store, &sid)
    };
    assert_eq!(mark(TaskState::Working, None), Some(ask::TaskSeen::Working));
    assert_eq!(
        mark(TaskState::Failed, Some(WHY_COULD_NOT_FINISH)),
        Some(ask::TaskSeen::Failed("could-not-finish".to_owned()))
    );
    assert_eq!(mark(TaskState::Working, None), Some(ask::TaskSeen::Working));
    assert_eq!(
        mark(TaskState::Exited, Some(WHY_EXIT)),
        Some(ask::TaskSeen::Failed("exit".to_owned()))
    );
    assert_eq!(mark(TaskState::Working, None), Some(ask::TaskSeen::Working));
    assert_eq!(
        mark(TaskState::Done, Some(WHY_LANDED)),
        Some(ask::TaskSeen::Ended)
    );
    assert_eq!(
        mark(TaskState::Exited, Some(WHY_EXIT)),
        Some(ask::TaskSeen::Ended),
        "the landing outlives the exit"
    );
    for why in ["startup", "held", "signin"] {
        assert_eq!(
            mark(TaskState::Stalled, Some(why)),
            Some(ask::TaskSeen::Working),
            "{why}"
        );
    }
    assert_eq!(task_bar_of(Some(TaskState::Failed)), Some("task-failed"));
}

/// EVERY `Over` THE LEASE RECORDS IS NOTED ONCE, by the process that moved it: a worker that never
/// started (no sid) and one whose host died (not here) have an end too. Only this host's own
/// worker has an Autopilot to turn Off at its expiry. NEGATIVE CONTROL: a start notes no end.
#[test]
fn every_over_the_lease_records_is_noted_once() {
    let state = aterm_tempfile::TempDir::new().expect("owned disk journal");
    let host = ask::HostId {
        pid: 1,
        start: String::new(),
    };
    let reading = aterm_sysprobe::volumes::Reading {
        mount: std::path::PathBuf::from("/u"),
        key: aterm_sysprobe::volumes::VolumeKey::Volume("dev:over".to_owned()),
        total: 2000 * GIB,
        free: 10 * GIB,
        files_free: Some(1 << 30),
        dev: 0,
        read_only: false,
    };
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(
        crate::session_store::SessionStore::default(),
    ));
    let words = |said: &ask::WorkerSaid| {
        let noted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = noted.clone();
        let note: Box<NoteFn> = Box::new(move |n: HostNote| {
            seen.lock().unwrap().push(match n {
                HostNote::DiskWorkerExpired { sid } => format!("expired {sid}"),
                HostNote::DiskWorkerOver {
                    sid, volume, why, ..
                } => {
                    format!("over {sid:?} {volume} {why}")
                }
                _ => "other".to_owned(),
            });
        });
        worker_moved(
            state.path(),
            0,
            "dev:over",
            said,
            &reading,
            &host,
            (&*note, &store),
            String::new,
        );
        noted.lock().unwrap().clone()
    };
    let over = |sid: Option<&str>, why: &str, here: bool| ask::WorkerSaid::Over {
        sid: sid.map(str::to_owned),
        why: why.to_owned(),
        rose: 0,
        target_met: None,
        here,
    };
    assert_eq!(
        words(&over(None, "failed:spawn", true)),
        ["over None dev:over failed:spawn"]
    );
    assert_eq!(
        words(&over(Some("s-1"), "expired", false)),
        ["over Some(\"s-1\") dev:over expired"],
        "another host's worker: no Off to send"
    );
    assert_eq!(
        words(&over(Some("s-2"), "expired", true)),
        ["expired s-2", "over Some(\"s-2\") dev:over expired"]
    );
}

#[test]
fn a_standing_word_is_journalled_once_and_every_move_each_time() {
    let state = aterm_tempfile::TempDir::new().expect("owned disk journal");
    let host = ask::HostId {
        pid: 1,
        start: String::new(),
    };
    let key = "dev:journal-once";
    let at = |said: &ask::Said| journal(state.path(), 0, key, said, Level::Short, 0, &host);
    at(&ask::Said::Quiet);
    assert!(
        rows(state.path()).is_empty(),
        "nothing to say while all is well"
    );
    at(&ask::Said::Freeing);
    at(&ask::Said::Freeing);
    at(&ask::Said::Opened {
        done: ask::Done::Spent,
        projected: None,
    });
    at(&ask::Said::Opened {
        done: ask::Done::Spent,
        projected: None,
    });
    at(&ask::Said::Settled);
    at(&ask::Said::Settled);
    at(&ask::Said::Quiet);
    let words: Vec<String> = rows(state.path())
        .iter()
        .filter_map(|r| {
            let v: aterm_json::Value = aterm_json::from_str(r).ok()?;
            Some(v.get("said")?.as_str()?.to_owned())
        })
        .collect();
    assert_eq!(words, ["freeing", "opened", "opened", "settled", "quiet"]);
}

#[test]
fn the_seat_rule_seats_a_supervised_claude_on_the_volume_and_bars_only_by_lasting_facts() {
    let seat = Facts {
        supervised: true,
        program: Some(Program::Claude),
        task: None,
        unfit: None,
        held: None,
        on_volume: Some(true),
    };
    assert_eq!(
        judge(&seat),
        Judged::Seat,
        "no task record: the worker's finding decides"
    );
    for (facts, why) in [
        (
            Facts {
                task: Some(TaskState::Working),
                ..seat
            },
            "managed",
        ),
        (
            Facts {
                task: Some(TaskState::Stalled),
                supervised: false,
                ..seat
            },
            "managed",
        ),
        (
            Facts {
                task: Some(TaskState::Done),
                ..seat
            },
            "task-done",
        ),
        (
            Facts {
                task: Some(TaskState::Exited),
                supervised: false,
                program: None,
                on_volume: None,
                ..seat
            },
            "task-exited",
        ),
        (
            Facts {
                unfit: Some("finished"),
                ..seat
            },
            "finished",
        ),
        (
            Facts {
                program: Some(Program::Codex),
                ..seat
            },
            "not-claude",
        ),
        (
            Facts {
                on_volume: Some(false),
                ..seat
            },
            "off-volume",
        ),
        (
            Facts {
                task: Some(TaskState::Done),
                held: Some(ask::HeldBy::Limit),
                ..seat
            },
            "task-done",
        ),
        (
            Facts {
                on_volume: Some(false),
                held: Some(ask::HeldBy::Box),
                ..seat
            },
            "off-volume",
        ),
    ] {
        assert_eq!(judge(&facts), Judged::Barred(why), "{facts:?}");
    }
    for why in [ask::HeldBy::Limit, ask::HeldBy::Box] {
        assert_eq!(
            judge(&Facts {
                held: Some(why),
                ..seat
            }),
            Judged::Held(why),
            "a seat but for what passes: held, not barred"
        );
    }
    for facts in [
        Facts {
            supervised: false,
            ..seat
        },
        Facts {
            program: None,
            ..seat
        },
        Facts {
            on_volume: None,
            ..seat
        },
        Facts {
            on_volume: None,
            held: Some(ask::HeldBy::Limit),
            ..seat
        },
        Facts {
            supervised: false,
            held: Some(ask::HeldBy::Box),
            ..seat
        },
    ] {
        assert_eq!(
            judge(&facts),
            Judged::Unseated,
            "nothing that lasts: an owed line stays owed while supervised: {facts:?}"
        );
    }
}

/// A session at a usage-limit wall its loop has open, before its reset and the loop's grace,
/// or at a box its loop escalated to its owner, cannot take a line before its delivery window
/// ends: the look holds it out of its seats. A seat rule that seated such a tab elected it,
/// often first (its checkout the largest), and the line waited out its window and was handed
/// on as undelivered, spending an election with nothing typed. The wall is read by the word
/// its agent publishes, `wall:<kind>`, of a kind its loop reads as a usage limit: the seat
/// rule once matched a `limited` no agent publishes, and held no session at a wall at all.
#[test]
fn a_session_at_an_unspent_wall_or_an_escalated_box_is_held() {
    use crate::disk_board::Wall;
    let wall = |until_s| Wall { until_s };
    let now = 1_000_000;
    let held = |word, escalated, wall: Option<&Wall>| {
        held_of(word, escalated, wall, None, false, Drafted::No, now)
    };
    for word in [
        "wall:usage-session",
        "wall:usage-weekly",
        "wall:model-bucket",
        "wall:spend",
        "wall:api-error",
    ] {
        assert_eq!(
            held(word, false, Some(&wall(Some(now + 1)))),
            Some(ask::HeldBy::Limit),
            "{word}"
        );
        assert_eq!(
            held(word, false, Some(&wall(None))),
            Some(ask::HeldBy::Limit),
            "{word}: no reset known: held until the loop closes the wall"
        );
        assert_eq!(
            held(word, false, Some(&wall(Some(now)))),
            None,
            "{word}: spent: the loop rests there, and the line can go"
        );
        assert_eq!(
            held(word, false, None),
            None,
            "{word}: no wall the loop has open (an API error that is no rate limit)"
        );
    }
    for word in ["wall:context", "wall:overloaded", "wall:memory"] {
        assert_eq!(
            held(word, false, Some(&wall(None))),
            None,
            "{word}: no usage limit's wall"
        );
    }
    assert_eq!(
        held("idle", false, Some(&wall(None))),
        None,
        "past the wall: its agent no longer reads limited"
    );
    for word in ["prompt", "question"] {
        assert_eq!(held(word, true, None), Some(ask::HeldBy::Box), "{word}");
        assert_eq!(
            held(word, false, None),
            None,
            "{word}: a box a rule answers is no person's to wait on"
        );
    }
    assert_eq!(held("busy", true, None), None);
}

/// What this process's passes freed is kept for the lease until a step lands it: a step the
/// lease's lock kept from landing reports it again at the next, merged past the lease's own
/// bound so nothing is lost, and a landed step forgets it.
#[test]
fn a_pass_the_lease_has_yet_to_record_is_reported_until_a_step_records_it() {
    let key = "dev:unrecorded-passes-test";
    let at = |ended_ms, bytes| ask::Reclaimed { ended_ms, bytes };
    assert_eq!(passes_unrecorded(key, &[at(10, 1)]), [at(10, 1)]);
    assert_eq!(
        passes_unrecorded(key, &[at(20, 2)]),
        [at(10, 1), at(20, 2)],
        "the step that missed the lease reports its pass again"
    );
    assert!(
        passes_unrecorded("dev:another-volume", &[]).is_empty(),
        "by volume"
    );
    let more: Vec<ask::Reclaimed> = (0..40).map(|i| at(100 + i, 1)).collect();
    let kept = passes_unrecorded(key, &more);
    assert_eq!(kept.len(), ask::MAX_RECLAIMED);
    assert_eq!(
        kept.iter().map(|r| r.bytes).sum::<u64>(),
        43,
        "nothing lost"
    );
    passes_recorded(key, &kept);
    assert!(
        passes_unrecorded(key, &[]).is_empty(),
        "recorded: forgotten"
    );
}

/// Another instance's pass holds the pass lock from its pass until its own step has recorded
/// what it freed: a look meanwhile reads it running ([`ask::Look::passing`]), and settles no
/// asked episode on the space it freed.
#[test]
fn a_look_reads_another_instances_pass_running_while_its_lock_is_held() {
    let home = aterm_tempfile::TempDir::new().expect("a scratch home");
    assert!(!another_pass_runs(home.path()), "nobody's pass");
    let lock = disk::reserve::take_pass(&disk::reserve::pass_lock_for(home.path()))
        .expect("the lock opens")
        .expect("nobody holds it");
    assert!(another_pass_runs(home.path()), "a pass holds it");
    drop(lock);
    assert!(!another_pass_runs(home.path()), "let go");
}

/// The loop's lasting withholds hold a session out of the seats for as long as they stand, as
/// a usage-limit wall does: its credits hold (`[harness] usage_credits = false`, the default),
/// which types nothing at its turn breaks until the limit resets, and a sign-in wall
/// (`wall:auth`), which only a person's browser moves. Seated, such a session was elected,
/// typed nothing for the delivery window and a grace, and was handed on as undelivered; as
/// the only seat, the episode then parked for six hours with nothing freed.
#[test]
fn a_session_its_loop_holds_for_credits_or_a_sign_in_is_held_until_it_is_past() {
    use crate::disk_board::Withhold;
    let now = 1_000_000;
    let held = |word, withhold: Option<&Withhold>| {
        held_of(word, false, None, withhold, false, Drafted::No, now)
    };
    let credits = |until_s| Withhold::Credits { until_s };
    assert_eq!(
        held("idle", Some(&credits(Some(now + 1)))),
        Some(ask::HeldBy::Credits)
    );
    assert_eq!(
        held("busy", Some(&credits(None))),
        Some(ask::HeldBy::Credits),
        "no lift time known: held until its loop finds it lifted"
    );
    assert_eq!(
        held("idle", Some(&credits(Some(now)))),
        None,
        "lifted: its loop types at the next turn break"
    );
    let auth = Withhold::Wall {
        wire: aterm_phase::WallKind::Auth.wire(),
    };
    assert_eq!(held("idle", Some(&auth)), Some(ask::HeldBy::SignIn));
    assert_eq!(
        held("wall:auth", None),
        Some(ask::HeldBy::SignIn),
        "its agent reads the sign-in wall before its loop decides a break there"
    );
    assert_eq!(
        held(
            "idle",
            Some(&Withhold::Wall {
                wire: "wall:overloaded"
            })
        ),
        Some(ask::HeldBy::Wall)
    );
    assert_eq!(held("idle", None), None);
    assert_eq!(
        ask::HeldBy::Credits.word(),
        "credits",
        "the lease and the journal name it"
    );
}

/// A session the owner holds (or a person took over: `status hold=1`), or whose composer
/// holds a draft that stands at a turn break (a rest, or a break of the agent's own background
/// work, where its agent reads busy), keeps to that turn break, and the loop refuses every
/// try there (`wait:held`, `wait:draft`, `background fact held: draft`): the look holds it,
/// as a wall or a box. Read as fit, it was nominated before a working session in the same GiB
/// (it rests at a turn break), elected, refused at every step of the wait for the delivery
/// window and a grace, and handed on as undelivered, spending an election with no line typed.
#[test]
fn a_held_session_or_one_whose_draft_stands_at_a_turn_break_is_held() {
    let now = 1_000_000;
    for word in ["idle", "busy", "prompt", "wall:usage-session"] {
        assert_eq!(
            held_of(word, false, None, None, true, Drafted::No, now),
            Some(ask::HeldBy::Hold),
            "{word}: the hold refuses any typed line"
        );
    }
    for drafted in [
        Drafted::Stands,
        Drafted::AtBackgroundBreak,
        Drafted::SinceBreak,
    ] {
        assert_eq!(
            held_of("idle", false, None, None, false, drafted, now),
            Some(ask::HeldBy::Draft),
            "{drafted:?}: at rest"
        );
    }
    assert_eq!(
        held_of(
            "busy",
            false,
            None,
            None,
            false,
            Drafted::AtBackgroundBreak,
            now
        ),
        Some(ask::HeldBy::Draft),
        "its turn over, only its own shell or monitor running: the loop's break refuses it"
    );
    assert_eq!(
        held_of("busy", false, None, None, false, Drafted::SinceBreak, now),
        Some(ask::HeldBy::Draft),
        "the draft that stood at its last break, through a turn the agent opened itself"
    );
    assert_eq!(
        held_of("busy", false, None, None, false, Drafted::Stands, now),
        None,
        "a draft typed while it works mid-turn goes with its turn"
    );
    for word in ["idle", "busy"] {
        assert_eq!(
            held_of(word, false, None, None, false, Drafted::No, now),
            None,
            "{word}"
        );
    }
    // What the look reads: the draft standing since its last look, and where.
    let typed = Draft::Text("land it".to_owned());
    let kept = |text: &str, at_break| Kept {
        text: text.to_owned(),
        at_break,
    };
    assert_eq!(
        Drafted::of_reading(&typed, Some(&kept("land it", false)), false),
        Drafted::Stands
    );
    assert_eq!(
        Drafted::of_reading(&typed, Some(&kept("land it", false)), true),
        Drafted::AtBackgroundBreak
    );
    assert_eq!(
        Drafted::of_reading(&typed, Some(&kept("land it", true)), true),
        Drafted::AtBackgroundBreak
    );
    assert_eq!(
        Drafted::of_reading(&typed, Some(&kept("land it", true)), false),
        Drafted::SinceBreak,
        "read at a break before, and the turn since took nothing from the composer"
    );
    assert_eq!(
        Drafted::of_reading(&typed, None, true),
        Drafted::No,
        "first read: it may be a person typing"
    );
    assert_eq!(
        Drafted::of_reading(&typed, Some(&kept("land", true)), false),
        Drafted::No,
        "still being typed"
    );
    assert_eq!(Drafted::of_reading(&Draft::Clear, None, true), Drafted::No);
    // What a look keeps: a text read at a turn break, and kept so while it stands unchanged.
    assert_eq!(Kept::after(None, "land it", true), kept("land it", true));
    assert_eq!(Kept::after(None, "land it", false), kept("land it", false));
    assert_eq!(
        Kept::after(Some(&kept("land it", true)), "land it", false),
        kept("land it", true),
        "unchanged since the break"
    );
    assert_eq!(
        Kept::after(Some(&kept("land it", true)), "land it now", false),
        kept("land it now", false),
        "typed into since, mid-turn: a new draft"
    );
    assert_eq!(
        Kept::after(Some(&kept("land it", false)), "land it", false),
        kept("land it", false),
        "typed mid-turn, never at a break"
    );
    let seat = Facts {
        supervised: true,
        program: Some(Program::Claude),
        task: None,
        unfit: None,
        held: None,
        on_volume: Some(true),
    };
    for why in [ask::HeldBy::Hold, ask::HeldBy::Draft] {
        assert_eq!(
            judge(&Facts {
                held: Some(why),
                ..seat
            }),
            Judged::Held(why),
            "neither seated nor barred"
        );
    }
    // A draft stands when the look before read the same typed text there.
    let text = Draft::Text("fix the flaky test".to_owned());
    assert!(text.stands(Some("fix the flaky test")));
    assert!(!text.stands(None), "first read: it may be a person typing");
    assert!(!text.stands(Some("fix the")), "still being typed");
    assert!(!Draft::Clear.stands(Some("fix the flaky test")));
    assert!(!Draft::Unread.stands(Some("fix the flaky test")));
}

/// The composer as the fenced write reads it: typed text is a draft; an empty composer, or
/// Claude Code's dim placeholder with the caret at its start, is none.
#[test]
fn a_draft_is_read_off_the_composer_as_the_fenced_write_reads_it() {
    let rule = "\u{2500}".repeat(60);
    let screen = |line: &str| vec![rule.clone(), format!("\u{276f} {line}"), rule.clone()];
    assert_eq!(
        draft_in(&screen("fix the flaky test"), (1, 20), |_| false),
        Draft::Text("fix the flaky test".to_owned())
    );
    assert_eq!(draft_in(&screen(""), (1, 2), |_| false), Draft::Clear);
    assert_eq!(
        draft_in(&screen("Try \"refactor this\""), (1, 2), |row| row == 1),
        Draft::Clear,
        "the dim placeholder"
    );
    assert_eq!(
        draft_in(&screen("Try \"refactor this\""), (1, 2), |_| false),
        Draft::Text("Try \"refactor this\"".to_owned()),
        "the same words typed"
    );
    assert_eq!(
        draft_in(&["$ ls".to_owned()], (0, 4), |_| false),
        Draft::Clear,
        "no composer drawn"
    );
}

/// Through the look: the owner's hold reads on the session at once, and its composer off its
/// own screen, a look's own rule keeping what it held for the next look (its [`Drafts`]), so the
/// second look to read the same typed text reads it standing; the take-over's rule before the
/// pass keeps nothing, and a cleared composer is forgotten.
#[test]
fn a_look_reads_the_owners_hold_and_a_draft_that_stands_since_its_last_look() {
    let drafts = Drafts::default();
    let mut sessions = crate::session_store::SessionStore::default();
    let h = crate::session_store::test_handle(9_300);
    sessions.register(h.clone());
    let sid = h.sid.as_str().to_owned();
    let sids = [sid.clone()];
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
    let key = "fsid:9300";
    let draw = |line: &str| {
        let rule = "\u{2500}".repeat(60);
        let col = line.chars().count() + 3;
        h.term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .process(
                format!("\x1b[2J\x1b[H{rule}\r\n\u{276f} {line}\r\n{rule}\x1b[2;{col}H").as_bytes(),
            );
    };
    let read = || {
        let r = read_sessions(&store, &sids, Some(&sid))
            .pop()
            .expect("the session");
        let last = drafts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&sid)
            .map(|k| k.text.clone());
        let stands = r.draft.stands(last.as_deref());
        (r.halted, r.draft, stands)
    };
    draw("");
    let clear = read();
    assert!(crate::fabric::apply_hold_for_test(
        &*h.ctx,
        Some(crate::fabric::Hold {
            reason: "-".into(),
            origin: "local".into(),
        })
    ));
    let halted = read().0;
    assert!(crate::fabric::apply_hold_for_test(&*h.ctx, None));
    draw("fix the flaky test");
    let first = read();
    let _ = rule(&store, &sids, key, &Left::NONE, &drafts, false);
    let before_pass = read();
    let _ = rule(&store, &sids, key, &Left::NONE, &drafts, true);
    let second = read();
    draw("");
    let _ = rule(&store, &sids, key, &Left::NONE, &drafts, true);
    let forgotten = drafts
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&sid)
        .is_none();
    assert_eq!(
        clear,
        (false, Draft::Clear, false),
        "CONTROL: nothing holds it"
    );
    assert!(halted, "the owner's hold: status hold=1");
    let text = Draft::Text("fix the flaky test".to_owned());
    assert_eq!(
        first,
        (false, text.clone(), false),
        "first read: maybe a person typing"
    );
    assert_eq!(
        before_pass,
        (false, text.clone(), false),
        "the take-over keeps nothing"
    );
    assert_eq!(second, (false, text, true), "it stood since the last look");
    assert!(forgotten, "cleared: forgotten");
}

/// Through the seat rule itself ([`rule`], [`judge_at`]) and the turn break ([`guards`],
/// [`AtBreak`]): a fit Claude Code tab resting at a turn break on the volume is seated; under
/// the owner's hold (`status hold=1`) it is held (`hold`), not seated, and a line owed it waits
/// there (`wait:hold`), unclaimed; lifted, it is seated again. With a draft typed in its prompt
/// it is seated at the look that first reads the draft (a person may be typing), held
/// (`draft`) from the next look that reads the same text, its owed line waiting the same way,
/// and seated again once the prompt is cleared. A seat rule that left the hold or the draft out
/// seated such a tab, which, at rest, outranked a working one in the same GiB, was elected, and
/// was refused at every turn break until the delivery window handed it on as undelivered.
#[cfg(unix)]
#[test]
fn the_seat_rule_holds_a_tab_on_hold_or_with_a_standing_draft_and_its_line_waits_unclaimed() {
    use aterm_sysprobe::volumes::VolumeKey;
    let drafts = Drafts::default();
    let mut sessions = crate::session_store::SessionStore::default();
    let h = crate::session_store::test_handle(9_400);
    sessions.register(h.clone());
    let sid = h.sid.as_str().to_owned();
    let sids = [sid.clone()];
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
    publishes(&h, "idle", Program::Claude);
    let draw = |line: &str| {
        let rule = "\u{2500}".repeat(60);
        let col = line.chars().count() + 3;
        h.term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .process(
                format!(
                    "\x1b]7;file://localhost/u/held-tab\x07\x1b[2J\x1b[H{rule}\r\n\u{276f} \
                     {line}\r\n{rule}\x1b[2;{col}H"
                )
                .as_bytes(),
            );
    };
    // The board places the tab on the volume asked about, as a pass does.
    let volume = VolumeKey::Fsid(9_400);
    let key = volume.spelled();
    placed(&volume, Some(&[h.local_id]));
    let read = || {
        let r = rule(&store, &sids, &key, &Left::NONE, &drafts, true);
        (
            r.seated
                .iter()
                .map(|s| s.seat.sid.clone())
                .collect::<Vec<_>>(),
            r.held,
        )
    };
    let hold = |on: bool| {
        assert!(crate::fabric::apply_hold_for_test(
            &*h.ctx,
            on.then(|| crate::fabric::Hold {
                reason: "-".into(),
                origin: "local".into(),
            })
        ));
    };
    draw("");
    let clear = read();
    hold(true);
    let on_hold = read();
    let judged_hold = judge_at(&store, &sid, &key, &drafts);
    hold(false);
    let lifted = read();
    draw("refactor the parser");
    let first = read();
    let second = read();
    let judged_draft = judge_at(&store, &sid, &key, &drafts);
    draw("");
    let cleared = read();
    placed(&volume, None);
    let seated = (vec![sid.clone()], Vec::new());
    let held = |why| (Vec::new(), vec![(sid.clone(), why)]);
    assert_eq!(clear, seated, "CONTROL: a fit tab at rest is seated");
    assert_eq!(on_hold, held(ask::HeldBy::Hold), "the owner's hold");
    assert_eq!(lifted, seated, "the hold lifted");
    assert_eq!(first, seated, "a draft read once: a person may be typing");
    assert_eq!(second, held(ask::HeldBy::Draft), "the same draft a look on");
    assert_eq!(cleared, seated, "the prompt cleared");
    assert_eq!(judged_hold, Judged::Held(ask::HeldBy::Hold));
    assert_eq!(judged_draft, Judged::Held(ask::HeldBy::Draft));
    // A line owed it waits at its turn break, unclaimed, so its time is not stretched.
    let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
    let host = ask::HostId {
        pid: 9,
        start: "start-9".to_owned(),
    };
    let now = now_ms();
    owed_at(state.path(), &key, &host, &sid, now);
    let before = ask::read(state.path()).expect("valid fixture lease");
    let owed = crate::disk_board::AskOwed {
        key: key.clone(),
        text: "the line".to_owned(),
    };
    for (judged, why) in [(judged_hold, "hold"), (judged_draft, "draft")] {
        let (unfit, held) = guards(judged, None, None);
        let at = AtBreak {
            state: state.path(),
            host: &host,
            sid: &sid,
            owed: &owed,
            now,
            unfit,
            held,
            free: 10 * GIB,
            level: Level::Short,
        };
        assert_eq!(
            at.step(
                || panic!("typed a tab {why} holds"),
                || panic!("measured at a tab {why} holds")
            ),
            format!("wait:{why}")
        );
    }
    assert_eq!(
        ask::read(state.path()).expect("valid fixture lease"),
        before,
        "unclaimed, and owed until the look gives it back to the episode"
    );
    assert_eq!(
        guards(Judged::Seat, None, None),
        (None, None),
        "CONTROL: a seat is typed"
    );
}

/// The board places sessions `locals` on volume `volume`, as a pass does; `None` takes them,
/// and the volume, off it again. Its callers read unix paths off a terminal (`OSC 7`).
#[cfg(unix)]
fn placed(volume: &aterm_sysprobe::volumes::VolumeKey, locals: Option<&[u64]>) {
    use aterm_sysprobe::volumes::Reading;
    let mut b = crate::disk_board::SHELF.board_lock();
    let Some(locals) = locals else {
        if let Some(at) = b.volumes.iter().position(|r| r.reading.key == *volume) {
            b.sessions.retain(|_, row| *row != at);
            b.volumes.remove(at);
            for row in b.sessions.values_mut() {
                if *row > at {
                    *row -= 1;
                }
            }
        }
        return;
    };
    b.volumes.push(crate::disk_board::VolumeRow {
        reading: Reading {
            mount: PathBuf::from("/u"),
            key: volume.clone(),
            total: 2000 * GIB,
            free: 10 * GIB,
            files_free: Some(1 << 30),
            dev: 0,
            read_only: false,
        },
        thresholds: disk::Config::default().thresholds(Some(2000 * GIB)),
        level: Level::Short,
        room: None,
        as_of: std::time::Instant::now(),
        as_of_unix: 0,
        reclaimed_here: true,
    });
    let at = b.volumes.len() - 1;
    for local in locals {
        b.sessions.insert(*local, at);
    }
}

/// Claude Code publishes `word` for session `h`.
#[cfg(unix)]
fn publishes(h: &crate::session_store::SessionHandle, word: &'static str, program: Program) {
    h.ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .publish_agent(
            word,
            None,
            None,
            Some(program),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
}

/// A tab whose turn is over with only its own background shell still running (a manager in
/// `aterm ctl await task` on its workers) reads busy, yet its loop offers its line at that
/// break as at an idle point, and refuses it there for a draft standing in its composer
/// (`background fact held: draft`). The look reads the same: from the second look that reads
/// the same text it holds the tab (`draft`), so a line owed it goes back to the episode at its
/// host's next look, at no election's cost. The look held a draft only at `idle`, so such a
/// pick stood owed through every refused break and was handed on as undelivered at its
/// deadline: one of the episode's elections spent, the tab never asked again that episode
/// once its draft was sent, and with no other seat the episode ran out and parked for its
/// cooldown. A draft queued while the agent works mid-turn goes with its turn: it holds
/// nothing, and a line owed it stays owed.
#[cfg(unix)]
#[test]
fn a_draft_standing_at_a_break_of_the_agents_own_background_work_gives_its_pick_back() {
    use aterm_sysprobe::volumes::VolumeKey;
    let drafts = Drafts::default();
    let mut sessions = crate::session_store::SessionStore::default();
    let h = crate::session_store::test_handle(9_500);
    sessions.register(h.clone());
    let sid = h.sid.as_str().to_owned();
    let sids = [sid.clone()];
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
    publishes(&h, "busy", Program::Claude);
    let draw = |status: &str, line: &str| {
        let rule = "\u{2500}".repeat(60);
        let col = line.chars().count() + 3;
        h.term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .process(
                format!(
                    "\x1b]7;file://localhost/u/manager\x07\x1b[2J\x1b[H\u{23fa} The workers \
                     run on; I land each as it reports.\r\n{status}\r\n{rule}\r\n\u{276f} \
                     {line}\r\n{rule}\x1b[4;{col}H"
                )
                .as_bytes(),
            );
    };
    let rows = || {
        let t = h.term.lock().unwrap_or_else(PoisonError::into_inner);
        (0..usize::from(t.rows()))
            .map(|r| crate::claude_lights::row_text(&t, r))
            .collect::<Vec<_>>()
    };
    let background =
        "\u{273b} Crunched for 9m 27s \u{b7} done 11:07 AM \u{b7} 1 shell still running";
    let working = "\u{273b} Crunching\u{2026} (3s \u{b7} esc to interrupt)";
    let volume = VolumeKey::Fsid(9_500);
    let key = volume.spelled();
    placed(&volume, Some(&[h.local_id]));
    // Each screen read by two looks a minute apart, the same draft typed in its prompt.
    let read = |status: &str| {
        draw(status, "");
        let _ = rule(&store, &sids, &key, &Left::NONE, &drafts, true);
        draw(status, "and then land the merge");
        let screen = rows();
        let first = rule(&store, &sids, &key, &Left::NONE, &drafts, true);
        let second = rule(&store, &sids, &key, &Left::NONE, &drafts, true);
        (screen, first, second)
    };
    let (bg_rows, bg_first, bg) = read(background);
    let (mid_rows, _, mid) = read(working);
    placed(&volume, None);
    assert!(
        aterm_phase::turn_over_background(&bg_rows),
        "CONTROL: the loop's own reader takes this screen as a break of its background work: \
         {bg_rows:#?}"
    );
    assert!(
        !aterm_phase::turn_over_background(&mid_rows),
        "CONTROL: mid-turn: {mid_rows:#?}"
    );
    let seated = |r: &Rule| {
        r.seated
            .iter()
            .map(|s| s.seat.sid.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        (seated(&bg_first), bg_first.held.clone()),
        (vec![sid.clone()], Vec::new()),
        "a draft read once: a person may be typing"
    );
    assert_eq!(
        (seated(&bg), bg.held.clone()),
        (Vec::new(), vec![(sid.clone(), ask::HeldBy::Draft)]),
        "the same draft a look on, at a break its loop would offer the line at"
    );
    assert_eq!(
        (seated(&mid), mid.held.clone()),
        (vec![sid.clone()], Vec::new()),
        "a draft queued mid-turn goes with its turn"
    );
    // A line owed it, at its host's next look, as each reading leaves the lease.
    let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
    let host = ask::HostId {
        pid: 10,
        start: "start-10".to_owned(),
    };
    let now = now_ms();
    let alive = |_: &ask::HostId| true;
    let next_look = |r: &Rule| {
        owed_at(state.path(), &key, &host, &sid, now);
        let seats: Vec<ask::Seat> = r.seated.iter().map(|s| s.seat.clone()).collect();
        let look = ask::Look {
            now_ms: now + 60_000,
            host: &host,
            key: &key,
            level: Level::Short,
            free: 10 * GIB,
            floor: 25 * GIB,
            files_free: None,
            files_floor: 0,
            done: Some(ask::Done::Spent),
            seats: &seats,
            supervised: &sids,
            barred: &r.barred,
            live: &r.live,
            held: &r.held,
            reclaimed: &[],
            passing: false,
            alive: &alive,
            ahead: ask::Ahead::default(),
        };
        let said = ask::transact(state.path(), &ask::LOCK_LADDER, |l| ask::step(l, &look))
            .expect("the lease is stepped")
            .expect("nobody holds its lock");
        (
            said,
            ask::read(state.path())
                .expect("valid fixture lease")
                .entry(&key)
                .cloned()
                .expect("entry"),
        )
    };
    let pick = ask::Pick {
        host: host.clone(),
        sid: sid.clone(),
    };
    let (said, entry) = next_look(&bg);
    assert_eq!(
        said,
        ask::Said::Returned {
            from: pick.clone(),
            why: ask::HeldBy::Draft
        },
        "{entry:?}"
    );
    assert!(entry.picks.is_empty(), "no election spent: {entry:?}");
    assert_eq!(
        entry.returned,
        std::slice::from_ref(&sid),
        "back in the episode"
    );
    assert_eq!(entry.released, 0, "{entry:?}");
    let (said, entry) = next_look(&mid);
    assert!(
        matches!(&said, ask::Said::Owed { pick: p, .. } if *p == pick),
        "CONTROL: mid-turn, owed until its turn break: {said:?} {entry:?}"
    );
}

/// A monitor the turn left running under a workflow's progress row its workflow outlived
/// (2.1.304 leaves the row drawn at its full bar,
/// `claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt`) is a break of the agent's
/// own background work to the look as to its loop, which offers its line there and refuses it
/// for a draft: a draft standing there holds the tab. Before, the look read the break by the
/// reader's one rule, the progress row's `a workflow running`, read ahead of the monitor's,
/// so it saw no break there and a draft standing held nothing. The row alone names nothing
/// the turn left: no break.
#[test]
fn a_monitor_under_an_outlived_progress_row_is_a_background_break_to_the_look() {
    const DRAFT: &str = "and then land the merge";
    let on = |monitor: bool| {
        let mut rows = aterm_phase::prompt::fixtures::screen(include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt"
        ));
        if monitor {
            rows.iter_mut()
                .find(|r| r.starts_with("✻ Brewed for 3m 20s"))
                .expect("the done row")
                .push_str(" · 1 monitor still running");
        }
        let caret = rows.iter().position(|r| r == "❯").expect("the composer");
        rows[caret] = format!("❯ {DRAFT}");
        let mut t =
            aterm_core::terminal::Terminal::new(u16::try_from(rows.len() + 2).expect("rows"), 120);
        t.process(
            format!(
                "\x1b[2J\x1b[H{}\x1b[{};{}H",
                rows.join("\r\n"),
                caret + 1,
                DRAFT.chars().count() + 3
            )
            .as_bytes(),
        );
        draft_on(&t)
    };
    let monitored = on(true);
    assert_eq!(monitored, (Draft::Text(DRAFT.to_owned()), true));
    // A second look reads it unchanged, as a look holds a draft by.
    let kept = Kept {
        text: DRAFT.to_owned(),
        at_break: false,
    };
    let drafted = Drafted::of_reading(&monitored.0, Some(&kept), monitored.1);
    assert_eq!(drafted, Drafted::AtBackgroundBreak);
    assert_eq!(
        held_of("busy", false, None, None, false, drafted, 0),
        Some(ask::HeldBy::Draft),
        "the draft holds the tab at its break"
    );
    assert_eq!(
        on(false),
        (Draft::Text(DRAFT.to_owned()), false),
        "CONTROL: the row alone"
    );
}

/// A manager in `aterm ctl await task` on its workers, with text left in its prompt, wakes
/// whenever a worker reports: its background command ends, Claude Code opens a turn of the
/// agent's own (`Background command "…" completed`), and that turn works for minutes before
/// it goes back to its await. The text stays in the prompt throughout. The look holds the tab
/// at the first break (`draft`), and its owed pick goes back to the episode; it holds it on
/// through the turn the agent's own wake opened, as the text that stood at the break stands
/// unchanged (that turn took nothing from the composer), whether or not the turn's first row
/// is still on screen; and at the next break. So the round, with nobody else to elect, waits
/// naming the draft, and never runs out; once the text is sent, the tab is seated and elected
/// again. The look read a draft that stood at a break as one typed mid-turn once the agent
/// woke: it seated the tab, the round elected it again, the next break held it, and a
/// session goes back to the episode only once, so it was released, and with no other seat
/// the episode ran out and parked for its cooldown, the text still in the prompt. A draft
/// typed while the agent works mid-turn, never yet left over a break, still holds nothing.
#[cfg(unix)]
#[test]
fn a_draft_that_stood_at_a_break_holds_its_tab_through_the_agents_own_wake_and_its_round_waits() {
    use aterm_sysprobe::volumes::VolumeKey;
    let drafts = Drafts::default();
    let mut sessions = crate::session_store::SessionStore::default();
    let h = crate::session_store::test_handle(9_700);
    sessions.register(h.clone());
    let sid = h.sid.as_str().to_owned();
    let sids = [sid.clone()];
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
    publishes(&h, "busy", Program::Claude);
    // Its screen: the transcript rows `above` its status row, then its composer holding `line`.
    let draw = |above: &[&str], status: &str, line: &str| {
        let rule = "\u{2500}".repeat(60);
        let col = line.chars().count() + 3;
        let row = above.len() + 3;
        let above: String = above.iter().map(|r| format!("{r}\r\n")).collect();
        h.term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .process(
                format!(
                    "\x1b]7;file://localhost/u/manager\x07\x1b[2J\x1b[H{above}{status}\r\n\
                     {rule}\r\n\u{276f} {line}\r\n{rule}\x1b[{row};{col}H"
                )
                .as_bytes(),
            );
    };
    let rows = || {
        let t = h.term.lock().unwrap_or_else(PoisonError::into_inner);
        (0..usize::from(t.rows()))
            .map(|r| crate::claude_lights::row_text(&t, r))
            .collect::<Vec<_>>()
    };
    let waits = "\u{23fa} The workers run on; I land each as it reports.";
    let woke = "\u{23fa} Background command \"aterm ctl await task @w1,@w2 since=0\" completed \
                (exit code 0)";
    let merging = "\u{23fa} w1 landed its work; merging it and running the checks.";
    let verifying = "\u{23fa} Bash(targo --unverified verify -p aterm-gui)";
    let merged = "\u{23fa} Merged w1. Waiting on w2.";
    let background =
        "\u{273b} Crunched for 9m 27s \u{b7} done 11:07 AM \u{b7} 1 shell still running";
    let working = "\u{273b} Crunching\u{2026} (2m 3s \u{b7} esc to interrupt)";
    let draft = "and then land the merge";
    let volume = VolumeKey::Fsid(9_700);
    let key = volume.spelled();
    placed(&volume, Some(&[h.local_id]));
    // One look a minute, each reading the screen drawn before it.
    let look = |above: &[&str], status: &str, line: &str| {
        draw(above, status, line);
        let screen = rows();
        (
            screen,
            rule(&store, &sids, &key, &Left::NONE, &drafts, true),
        )
    };
    let (_, typed) = look(&[waits], background, draft);
    let (break_rows, at_break) = look(&[waits], background, draft);
    let (wake_rows, woken) = look(&[waits, woke, merging], working, draft);
    let (later_rows, later) = look(&[verifying], working, draft);
    let (_, back) = look(&[merging, merged], background, draft);
    let sent_with = format!("\u{276f} {draft}");
    let (_, sent) = look(&[merged, &sent_with], working, "");
    let fresh = "also bump the version";
    let (_, fresh_first) = look(&[merged, &sent_with], working, fresh);
    let (_, fresh_stands) = look(&[merged, &sent_with], working, fresh);
    placed(&volume, None);
    assert!(
        aterm_phase::turn_over_background(&break_rows),
        "CONTROL: a break of its background work: {break_rows:#?}"
    );
    assert!(
        !aterm_phase::turn_over_background(&wake_rows)
            && !aterm_phase::turn_over_background(&later_rows),
        "CONTROL: its own wake's turn is no break: {wake_rows:#?} {later_rows:#?}"
    );
    assert_eq!(
        (
            aterm_phase::opened_by_own_wake(&wake_rows),
            aterm_phase::opened_by_own_wake(&later_rows)
        ),
        (Some(true), None),
        "CONTROL: its own wake opened the turn, and the screen stops saying so"
    );
    let seated = |r: &Rule| {
        r.seated
            .iter()
            .map(|s| s.seat.sid.clone())
            .collect::<Vec<_>>()
    };
    let draft_held = (Vec::new(), vec![(sid.clone(), ask::HeldBy::Draft)]);
    let read = |r: &Rule| (seated(r), r.held.clone());
    assert_eq!(
        read(&typed),
        (vec![sid.clone()], Vec::new()),
        "read once: a person may be typing"
    );
    assert_eq!(read(&at_break), draft_held, "at its background break");
    assert_eq!(
        read(&woken),
        draft_held,
        "its own wake: the draft stood at the break and the turn took nothing from it"
    );
    assert_eq!(
        read(&later),
        draft_held,
        "the turn's first row scrolled off: the same text still stands"
    );
    assert_eq!(read(&back), draft_held, "back at its break");
    assert_eq!(
        read(&sent),
        (vec![sid.clone()], Vec::new()),
        "the text sent: seated"
    );
    assert_eq!(
        (read(&fresh_first), read(&fresh_stands)),
        (
            (vec![sid.clone()], Vec::new()),
            (vec![sid.clone()], Vec::new())
        ),
        "CONTROL: a draft typed mid-turn, never left over a break, goes with its turn"
    );
    // The lease, owed to it since the look that read the draft once seated it, stepped by
    // its host at each look after.
    let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
    let host = ask::HostId {
        pid: 11,
        start: "start-11".to_owned(),
    };
    let t0 = now_ms();
    owed_at(state.path(), &key, &host, &sid, t0);
    let alive = |_: &ask::HostId| true;
    let step_at = |r: &Rule, at: i64| {
        let seats: Vec<ask::Seat> = r.seated.iter().map(|s| s.seat.clone()).collect();
        let look = ask::Look {
            now_ms: at,
            host: &host,
            key: &key,
            level: Level::Short,
            free: 10 * GIB,
            floor: 25 * GIB,
            files_free: None,
            files_floor: 0,
            done: Some(ask::Done::Spent),
            seats: &seats,
            supervised: &sids,
            barred: &r.barred,
            live: &r.live,
            held: &r.held,
            reclaimed: &[],
            passing: false,
            alive: &alive,
            ahead: ask::Ahead::default(),
        };
        let said = ask::transact(state.path(), &ask::LOCK_LADDER, |l| ask::step(l, &look))
            .expect("the lease is stepped")
            .expect("nobody holds its lock");
        (
            said,
            ask::read(state.path())
                .expect("valid fixture lease")
                .entry(&key)
                .cloned()
                .expect("entry"),
        )
    };
    let minute = 60_000;
    let pick = ask::Pick {
        host: host.clone(),
        sid: sid.clone(),
    };
    let (said, _) = step_at(&at_break, t0 + minute);
    assert_eq!(
        said,
        ask::Said::Returned {
            from: pick.clone(),
            why: ask::HeldBy::Draft
        }
    );
    let waiting = ask::Said::Waiting {
        held: vec![sid.clone()],
        why: vec![ask::HeldBy::Draft],
    };
    let round_past = t0 + minute + ask::NOMINATE_MS;
    let (said, _) = step_at(&woken, t0 + 2 * minute);
    assert!(
        matches!(said, ask::Said::Nominating { seat: None, .. }),
        "its round gathers, the tab held: {said:?}"
    );
    for (r, at, what) in [
        (&later, round_past, "through its own wake's turn"),
        (&back, round_past + minute, "back at its break"),
    ] {
        let (said, entry) = step_at(r, at);
        assert_eq!(said, waiting, "{what}: {entry:?}");
        assert!(
            entry.picks.is_empty() && entry.released == 0,
            "{what}: no election spent, none released: {entry:?}"
        );
    }
    let (said, entry) = step_at(&sent, round_past + 2 * minute);
    assert_eq!(
        said,
        ask::Said::Elected {
            pick: pick.clone(),
            held: 0
        },
        "the text sent: elected again, {entry:?}"
    );
}

/// The asked tab's line marks its own checkout's builds `(this tab's)` only while no other
/// session of this process works in that checkout, supervised or not: several Claude Code
/// tabs in `~/aterm` is ordinary, and they build in one cache, so the line says `(this tab's
/// checkout, shared)`, and never tells the agent a cache other sessions build in is its own.
/// A tab in a worktree nested in the checkout works in its own checkout, and shares nothing
/// of it. The seat's rank is unchanged: it holds what its checkout's builds hold.
#[cfg(unix)]
#[test]
fn a_seat_whose_checkout_another_session_works_in_is_told_its_builds_are_shared() {
    use aterm_sysprobe::volumes::VolumeKey;
    let asked = crate::session_store::test_handle(9_600);
    let shell = crate::session_store::test_handle(9_601);
    let nested = crate::session_store::test_handle(9_602);
    let peer = crate::session_store::test_handle(9_603);
    let cd = |h: &crate::session_store::SessionHandle, dir: &str| {
        h.term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .process(format!("\x1b]7;file://localhost{dir}\x07").as_bytes());
    };
    cd(&asked, "/u/aterm/crates/x");
    cd(&shell, "/u/aterm");
    cd(&nested, "/u/aterm/.claude/worktrees/w/crates");
    cd(&peer, "/u/aterm/crates/y");
    publishes(&asked, "idle", Program::Claude);
    publishes(&nested, "busy", Program::Claude);
    publishes(&peer, "busy", Program::Claude);
    let sid = asked.sid.as_str().to_owned();
    let peer_sid = peer.sid.as_str().to_owned();
    let volume = VolumeKey::Fsid(9_600);
    let key = volume.spelled();
    placed(
        &volume,
        Some(&[
            asked.local_id,
            shell.local_id,
            nested.local_id,
            peer.local_id,
        ]),
    );
    let holders = [
        Hog {
            built_s_ago: None,
            path: PathBuf::from("/u/Library/Caches/aterm/cargo-build.noindex/a"),
            bytes: 61 * GIB,
            bytes_partial: false,
            why: String::new(),
            serves: Some(PathBuf::from("/u/aterm")),
        },
        Hog {
            built_s_ago: None,
            path: PathBuf::from("/u/trust/build"),
            bytes: 30 * GIB,
            bytes_partial: false,
            why: String::new(),
            serves: Some(PathBuf::from("/u/trust")),
        },
    ];
    let roots = ["/u/aterm", "/u/aterm/.claude/worktrees/w", "/u/trust"];
    let root = |dir: &Path| roots.iter().any(|r| dir == Path::new(r));
    let there = |_: &Path| true;
    let left = Left {
        holders: &holders,
        home: Some(Path::new("/u")),
        root: &root,
        there: &there,
    };
    // The asked tab's seat and its line, with `with` the other live sessions and `supervised`
    // those the harness supervises.
    let seat = |with: &[&crate::session_store::SessionHandle], supervised: &[String]| {
        let mut sessions = crate::session_store::SessionStore::default();
        sessions.register(asked.clone());
        for h in with {
            sessions.register((*h).clone());
        }
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        let r = rule(&store, supervised, &key, &left, &Drafts::default(), false);
        let s = r
            .seated
            .into_iter()
            .find(|s| s.seat.sid == sid)
            .expect("the asked tab is seated");
        let text = ask::line(&ask::Line {
            mount: Path::new("/System/Volumes/Data"),
            total: 1000 * GIB,
            free: 18 * GIB,
            floor: 25 * GIB,
            emergency: false,
            done: ask::Done::Spent,
            holders: &holders,
            checkout: s.checkout.as_deref(),
            shared: s.shared,
            home: Some(Path::new("/u")),
            projected: None,
        });
        (s.seat.held, s.shared, text)
    };
    let one = std::slice::from_ref(&sid);
    let alone = seat(&[], one);
    let beside_nested = seat(&[&nested], &[sid.clone(), nested.sid.as_str().to_owned()]);
    let beside_shell = seat(&[&shell, &nested], one);
    let beside_peer = seat(&[&peer], &[sid.clone(), peer_sid.clone()]);
    placed(&volume, None);
    let own = "~/aterm's builds 61.0 GiB (this tab's); $HOME/trust's builds 30.0 GiB.";
    let shared =
        "~/aterm's builds 61.0 GiB (this tab's checkout, shared); $HOME/trust's builds 30.0 GiB.";
    assert_eq!((alone.0, alone.1), (61 * GIB, false), "CONTROL: alone");
    assert!(alone.2.contains(own), "{}", alone.2);
    assert_eq!(
        (beside_nested.0, beside_nested.1),
        (61 * GIB, false),
        "a worktree nested in the checkout is its own checkout"
    );
    assert!(beside_nested.2.contains(own), "{}", beside_nested.2);
    assert_eq!(
        (beside_shell.0, beside_shell.1),
        (61 * GIB, true),
        "a shell in the same checkout, unsupervised: shared, and the seat ranks as before"
    );
    assert!(beside_shell.2.contains(shared), "{}", beside_shell.2);
    assert!(
        !beside_shell.2.contains("(this tab's)"),
        "{}",
        beside_shell.2
    );
    assert_eq!(
        (beside_peer.0, beside_peer.1),
        (61 * GIB, true),
        "another Claude Code tab working in the same checkout"
    );
    assert!(beside_peer.2.contains(shared), "{}", beside_peer.2);
    for text in [&alone.2, &beside_shell.2, &beside_peer.2] {
        assert!(text.chars().count() <= ask::LINE_CAP, "{text}");
    }
}

/// The composer's first glyph is read past a caret the program draws over it, as the fenced
/// write reads it: Claude Code draws its suggestion's first glyph inverse and the rest dim, and
/// a typed draft's first glyph inverse and the rest plain.
#[test]
fn the_composers_first_glyph_is_read_past_a_drawn_caret() {
    use aterm_core::grid::CellFlags;
    let at = |cells: Vec<(usize, CellFlags)>| {
        move |col: usize| cells.iter().find(|(c, _)| *c == col).map(|(_, f)| *f)
    };
    let (dim, plain, inverse) = (CellFlags::DIM, CellFlags::empty(), CellFlags::INVERSE);
    assert!(glyph_dim(at(vec![(2, dim)])), "dim at the caret");
    assert!(
        glyph_dim(at(vec![(2, inverse), (3, dim)])),
        "a suggestion under a drawn caret"
    );
    assert!(
        !glyph_dim(at(vec![(2, inverse), (3, plain)])),
        "a typed draft under a drawn caret"
    );
    assert!(
        glyph_dim(at(vec![
            (2, inverse.union(CellFlags::WIDE)),
            (3, plain),
            (4, dim),
        ])),
        "past a wide glyph"
    );
    assert!(!glyph_dim(at(vec![(2, plain)])));
    assert!(!glyph_dim(at(Vec::new())), "no cell");
}

/// A look that runs the episode out raises the owner's `disk.low` itself only when it reads
/// the volume short against its own floor. A look that read it at or above its floor (an
/// episode another host's higher floor held open) raised "the home volume is still short",
/// and the next look, finding it plenty, cleared it: a false alert kept as a message record.
#[test]
fn a_look_that_runs_the_episode_out_raises_disk_low_only_reading_the_volume_short() {
    let out = ask::Said::Out {
        last: Some(ask::Miss::NothingFreed),
        asked: true,
    };
    let spent = Some(ask::Done::Spent);
    let floor = 32 << 30;
    assert!(out_raises_low(&out, Level::Short, floor, spent));
    assert!(out_raises_low(&out, Level::Emergency, floor, spent));
    for level in [Level::Low, Level::Ok, Level::Unknown] {
        assert!(
            !out_raises_low(&out, level, floor, spent),
            "{level:?}: not read short against this host's floor"
        );
    }
    assert!(
        !out_raises_low(&out, Level::Short, 0, spent),
        "the floor at 0"
    );
    assert!(
        !out_raises_low(&out, Level::Short, floor, Some(ask::Done::CleanupOff)),
        "removal off: its own line"
    );
    assert!(!out_raises_low(
        &ask::Said::Spent,
        Level::Short,
        floor,
        spent
    ));
}

/// The look reads a box as escalated by the supervisor's own attention entry on the session,
/// which its loop writes as it asks the owner, and not by the badge of a usage limit it
/// raised, which a wall the board holds says instead.
#[test]
fn a_look_reads_a_box_its_loop_escalated_and_not_a_limits_badge() {
    let mut sessions = crate::session_store::SessionStore::default();
    let h = crate::session_store::test_handle(9_200);
    sessions.register(h.clone());
    let sid = h.sid.as_str().to_owned();
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
    h.ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .publish_agent(
            "prompt",
            None,
            None,
            Some(Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    let read = |badge: Option<&str>| {
        h.ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_attention_owned(
                aterm_agent::supervise::ATTENTION_OWNER,
                badge.map(str::to_owned),
            )
            .expect("a slot");
        let r = read_sessions(&store, std::slice::from_ref(&sid), Some(&sid))
            .pop()
            .expect("the session");
        (r.word, r.escalated)
    };
    assert_eq!(read(None), ("prompt", false), "CONTROL: nobody asked");
    assert_eq!(read(Some("approve: rm -rf target")), ("prompt", true));
    let limit = format!("{} resets 3pm", aterm_agent::supervise::ATTENTION_PREFIX);
    assert_eq!(read(Some(&limit)), ("prompt", false), "a limit's badge");
}

#[test]
fn a_look_and_a_turn_break_bar_a_done_an_exited_and_an_unfit_session_alike() {
    use crate::task_record::{TaskMark, mark_session_task};
    let mut sessions = crate::session_store::SessionStore::default();
    let handles: Vec<_> = (0..5u64)
        .map(|local| {
            let h = crate::session_store::test_handle(9_000 + local);
            h.ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .set_agent_reader_program_for_test(Some(Program::Claude));
            sessions.register(h.clone());
            h
        })
        .collect();
    let sid = |i: usize| handles[i].sid.as_str().to_owned();
    let mark = |i: usize, state: TaskState| {
        mark_session_task(&handles[i].ctx, handles[i].local_id, &TaskMark::new(state))
            .expect("unfenced");
    };
    // 0: its task landed; 1: its agent exited, so it left the supervised roster; 2: its
    // worker found it unfit; 3: a manager's worker, its task working, its turn break not yet
    // found; 4: no task record, not supervised.
    mark(0, TaskState::Done);
    mark(1, TaskState::Exited);
    mark(3, TaskState::Working);
    let sids = [sid(0), sid(2), sid(3)];
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
    crate::disk_board::SHELF.board_lock().unfit.insert(
        sid(2),
        crate::disk_board::Unfit {
            own: None,
            theirs: Some("exchange"),
        },
    );
    let key = "fsid:9000";
    let found = rule(&store, &sids, key, &Left::NONE, &Drafts::default(), false);
    let bars: Vec<Option<&'static str>> = (0..5).map(|i| bar_at(&store, &sid(i), key)).collect();
    crate::disk_board::SHELF.board_lock().unfit.remove(&sid(2));
    let mut barred = found.barred.clone();
    barred.sort();
    let mut want = vec![sid(0), sid(1), sid(2), sid(3)];
    want.sort();
    assert_eq!(barred, want, "what the look releases an owed line of");
    assert!(
        found.seated.is_empty(),
        "no directory on the volume was read"
    );
    for i in 0..5 {
        assert!(found.live.contains(&sid(i)), "every live session: {i}");
    }
    assert_eq!(
        bars,
        [
            Some("task-done"),
            Some("task-exited"),
            Some("exchange"),
            Some("managed"),
            None
        ],
        "the turn break bars what the look bars"
    );
    assert_eq!(bar_at(&store, "s-not-here", key), None);
}

/// An earlier disk worker that still runs keeps the next from starting on this host
/// ([`worker_ok_here`]): one stalled at its start or before Claude Code ran, or working within
/// [`ask::WORKER_MS`] of its last change, until it is done, exits, is closed or a person
/// switches its Autopilot Off; a session in another folder keeps nothing.
///
/// NEGATIVE CONTROL: with `earlier_worker` answering `None` (the look's old
/// `worker_ok: true`), the first assertion failed ("the disk worker stands at a sign-in
/// screen").
#[test]
fn an_earlier_disk_worker_that_still_runs_keeps_the_next_from_starting() {
    use crate::task_record::{TaskMark, mark_session_task};
    use aterm_agent::supervise::Autopilot;
    let worker_folder = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        ask::WORKER_FOLDER
    );
    let mut sessions = crate::session_store::SessionStore::default();
    let handles: Vec<_> = (0..3u64)
        .map(|local| {
            let h = crate::session_store::test_handle(9_100 + local);
            sessions.register(h.clone());
            h
        })
        .collect();
    let mark = |i: usize, state: TaskState, why: Option<&'static str>| {
        let mark = TaskMark {
            why,
            ..TaskMark::new(state)
        };
        mark_session_task(&handles[i].ctx, handles[i].local_id, &mark).expect("unfenced");
    };
    let trust = |i: usize, folder: &str| {
        handles[i]
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_task_worker(Program::Claude, Some(folder.to_owned()));
    };
    let autopilot = |i: usize, mode: Autopilot| {
        handles[i]
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_autopilot(Some(mode.word().to_owned()), None);
    };
    // 0: the disk worker, started under Land; 1: a worker in a folder of its own; 2: no task
    // record.
    mark(0, TaskState::Working, None);
    trust(0, &worker_folder);
    autopilot(0, Autopilot::Land);
    mark(1, TaskState::Stalled, Some("startup"));
    trust(1, "/Users//me/project");
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
    let disk_worker = handles[0].sid.as_str().to_owned();
    let now = now_ms();

    mark(0, TaskState::Stalled, Some("startup"));
    assert!(
        !worker_ok_here(&store, now),
        "the disk worker stands at a sign-in screen"
    );
    assert_eq!(
        earlier_worker(&store, now),
        Some((disk_worker.clone(), "stalled why=startup".to_owned())),
        "the other stuck one is not a disk worker"
    );

    mark(0, TaskState::Stalled, Some("install"));
    assert_eq!(
        earlier_worker(&store, now),
        Some((disk_worker.clone(), "stalled why=install".to_owned())),
        "stopped before Claude Code ran, at a shell no agent roster names"
    );

    autopilot(0, Autopilot::Off);
    assert!(
        worker_ok_here(&store, now),
        "a person switched its Autopilot Off: it is theirs now"
    );
    autopilot(0, Autopilot::Land);

    mark(0, TaskState::Working, None);
    assert_eq!(
        earlier_worker(&store, now),
        Some((disk_worker.clone(), "working".to_owned())),
        "it works in the one worker folder"
    );
    let working_since = handles[0]
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .map(|record| i64::try_from(record.since_unix_ms).expect("a unix ms"))
        .expect("its task record");
    assert!(
        !worker_ok_here(&store, working_since + ask::WORKER_MS - 1),
        "within the time any worker is given, it holds the next back"
    );
    assert!(
        worker_ok_here(&store, working_since + ask::WORKER_MS),
        "working past the time any worker is given, it no longer holds the next back"
    );

    for ended in [TaskState::Done, TaskState::Exited] {
        mark(0, ended, None);
        assert!(worker_ok_here(&store, now), "{ended:?}");
    }

    mark(0, TaskState::Stalled, Some("startup"));
    assert!(!worker_ok_here(&store, now));
    store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .deregister_local(handles[0].local_id);
    assert!(worker_ok_here(&store, now), "a closed worker keeps nothing");
}

#[test]
fn a_session_holds_what_its_own_checkout_holds_and_a_line_typed_here_is_never_typed_twice() {
    let hog = |path: &str, gib: u64, serves: Option<&str>| Hog {
        built_s_ago: None,
        path: PathBuf::from(path),
        bytes: gib << 30,
        bytes_partial: false,
        why: String::new(),
        serves: serves.map(PathBuf::from),
    };
    let holders = [
        hog("/c/aterm-build", 40, Some("/u/aterm")),
        hog("/u/aterm/target", 2, Some("/u/aterm")),
        hog("/u/trust/build", 90, Some("/u/trust")),
        hog("/u/loose/target", 7, None),
        hog(
            "/u/aterm/.claude/worktrees/w/target",
            3,
            Some("/u/aterm/.claude/worktrees/w"),
        ),
    ];
    let roots = ["/u/aterm", "/u/aterm/.claude/worktrees/w", "/u/trust"];
    let root = |dir: &Path| roots.iter().any(|r| dir == Path::new(r));
    let there = |_: &Path| true;
    let home = Some(Path::new("/u"));
    let held_under = |cwd: &str| {
        held_in(
            &holders,
            checkout_of(&holders, Path::new(cwd), home, &root, &there).as_deref(),
        )
    };
    assert_eq!(held_under("/u/aterm/crates/x"), 42 << 30);
    assert_eq!(held_under("/u/atermx"), 0);
    assert_eq!(held_under("/u/loose"), 0);
    // A worktree nested in a checkout is its own checkout, not the one around it.
    let nested = Path::new("/u/aterm/.claude/worktrees/w/crates");
    assert_eq!(held_under("/u/aterm/.claude/worktrees/w/crates"), 3 << 30);
    assert_eq!(
        checkout_of(&holders, nested, home, &root, &there).as_deref(),
        Some(Path::new("/u/aterm/.claude/worktrees/w"))
    );
    assert_eq!(
        checkout_of(&holders, Path::new("/u/aterm"), home, &root, &there).as_deref(),
        Some(Path::new("/u/aterm"))
    );
    let key = "dev:typed-here";
    assert!(!typed_here(key, "s-a", 1_000));
    TYPED.lock().unwrap_or_else(PoisonError::into_inner).insert(
        key.to_owned(),
        Typed {
            sid: "s-a".to_owned(),
            at_ms: 1_000,
            free: 0,
        },
    );
    assert!(typed_here(key, "s-a", 1_000 + ask::COOLDOWN_MS - 1));
    assert!(!typed_here(key, "s-b", 2_000));
    assert!(!typed_here(key, "s-a", 1_000 + ask::COOLDOWN_MS));
}

#[test]
fn a_worktree_nested_in_a_checkout_with_no_holder_of_its_own_holds_nothing_of_the_checkouts() {
    let hog = |path: &str, gib: u64, serves: &str| Hog {
        built_s_ago: None,
        path: PathBuf::from(path),
        bytes: gib << 30,
        bytes_partial: false,
        why: String::new(),
        serves: Some(PathBuf::from(serves)),
    };
    // The pass's holders: the enclosing checkout's builds, and none of the worktree's (past
    // the five largest, or its idle build already reclaimed).
    let holders = [
        hog(
            "/u/Library/Caches/aterm/cargo-build.noindex/a",
            61,
            "/u/aterm",
        ),
        hog("/u/trust/build", 30, "/u/trust"),
    ];
    let roots = ["/u/aterm", "/u/aterm/.claude/worktrees/v", "/u/trust"];
    let looked = Mutex::new(Vec::<PathBuf>::new());
    let root = |dir: &Path| {
        looked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(dir.to_path_buf());
        roots.iter().any(|r| dir == Path::new(r))
    };
    let there = |_: &Path| true;
    let home = Path::new("/u");
    let looked_at = || std::mem::take(&mut *looked.lock().unwrap_or_else(PoisonError::into_inner));
    let said = |checkout: Option<&Path>| {
        ask::line(&ask::Line {
            mount: Path::new("/System/Volumes/Data"),
            total: 1000 << 30,
            free: 18 << 30,
            floor: 25 << 30,
            emergency: false,
            done: ask::Done::Spent,
            holders: &holders,
            checkout,
            shared: false,
            home: Some(home),
            projected: None,
        })
    };
    let wt = Path::new("/u/aterm/.claude/worktrees/v/crates/x");
    let checkout = checkout_of(&holders, wt, Some(home), &root, &there);
    assert_eq!(
        checkout.as_deref(),
        Some(Path::new("/u/aterm/.claude/worktrees/v"))
    );
    assert_eq!(held_in(&holders, checkout.as_deref()), 0);
    let text = said(checkout.as_deref());
    assert!(text.contains("~/aterm's builds 61.0 GiB"), "{text}");
    assert!(!text.contains("(this tab's)"), "{text}");
    // It looks only between the tab's directory and the checkout around it, and stops at the
    // first root.
    assert_eq!(
        looked_at(),
        [
            PathBuf::from("/u/aterm/.claude/worktrees/v/crates/x"),
            PathBuf::from("/u/aterm/.claude/worktrees/v/crates"),
            PathBuf::from("/u/aterm/.claude/worktrees/v"),
        ]
    );
    // The worktree's root itself is its own too.
    let at_root = checkout_of(
        &holders,
        Path::new("/u/aterm/.claude/worktrees/v"),
        Some(home),
        &root,
        &there,
    );
    assert_eq!(held_in(&holders, at_root.as_deref()), 0);
    looked_at();
    // A tab in the checkout itself still holds its builds, and is told they are its own.
    for cwd in ["/u/aterm", "/u/aterm/crates/x"] {
        let checkout = checkout_of(&holders, Path::new(cwd), Some(home), &root, &there);
        assert_eq!(checkout.as_deref(), Some(Path::new("/u/aterm")), "{cwd}");
        assert_eq!(held_in(&holders, checkout.as_deref()), 61 << 30, "{cwd}");
        let text = said(checkout.as_deref());
        assert!(
            text.contains("~/aterm's builds 61.0 GiB (this tab's)"),
            "{text}"
        );
        assert!(!looked_at().contains(&PathBuf::from("/u/aterm")), "{cwd}");
    }
    // No holder serves a checkout around the tab: nothing is read, and nothing is its own.
    assert_eq!(
        checkout_of(&holders, Path::new("/u/other/x"), Some(home), &root, &there),
        None
    );
    assert!(looked_at().is_empty());
    // Nothing above `$HOME` is read for a root.
    let wide = [hog("/c/root-build", 5, "/")];
    assert_eq!(
        checkout_of(
            &wide,
            Path::new("/h/u/p/q"),
            Some(Path::new("/h/u")),
            &root,
            &there
        )
        .as_deref(),
        Some(Path::new("/"))
    );
    assert_eq!(
        looked_at(),
        [
            PathBuf::from("/h/u/p/q"),
            PathBuf::from("/h/u/p"),
            PathBuf::from("/h/u")
        ]
    );
}

#[test]
fn a_worktree_removed_under_its_session_holds_nothing_of_the_checkout_around_it() {
    // ~/aterm's builds are the one holder left; the tab's shell last said it works in a
    // worktree nested in ~/aterm that has since been removed (its session landed it), so no
    // `.git` is left to find between its directory and ~/aterm.
    let holders = [Hog {
        built_s_ago: None,
        path: PathBuf::from("/u/Library/Caches/aterm/cargo-build.noindex/a"),
        bytes: 61 << 30,
        bytes_partial: false,
        why: String::new(),
        serves: Some(PathBuf::from("/u/aterm")),
    }];
    let roots = ["/u/aterm", "/u/aterm/.claude/worktrees/v"];
    let root = |dir: &Path| roots.iter().any(|r| dir == Path::new(r));
    let gone = Path::new("/u/aterm/.claude/worktrees/gone");
    let asked = Mutex::new(Vec::<PathBuf>::new());
    let there = |dir: &Path| {
        asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(dir.to_path_buf());
        !dir.starts_with(gone) && !dir.starts_with("/u/aterm/.claude/worktrees/v/tmp")
    };
    let asked_of = || std::mem::take(&mut *asked.lock().unwrap_or_else(PoisonError::into_inner));
    let home = Path::new("/u");
    let of = |cwd: &str| checkout_of(&holders, Path::new(cwd), Some(home), &root, &there);
    for cwd in [
        "/u/aterm/.claude/worktrees/gone/crates",
        "/u/aterm/.claude/worktrees/gone",
    ] {
        let checkout = of(cwd);
        assert_eq!(checkout, None, "{cwd}");
        assert_eq!(held_in(&holders, checkout.as_deref()), 0, "{cwd}");
        let text = ask::line(&ask::Line {
            mount: Path::new("/System/Volumes/Data"),
            total: 1000 << 30,
            free: 18 << 30,
            floor: 25 << 30,
            emergency: false,
            done: ask::Done::Spent,
            holders: &holders,
            checkout: checkout.as_deref(),
            shared: false,
            home: Some(home),
            projected: None,
        });
        assert!(text.contains("~/aterm's builds 61.0 GiB"), "{text}");
        assert!(!text.contains("(this tab's)"), "{text}");
        // Only the tab's own directory is asked, once the walk found no root.
        assert_eq!(asked_of(), [PathBuf::from(cwd)], "{cwd}");
    }
    // A directory removed inside a worktree that is still there is that worktree's: its
    // `.git` is found, and nothing more is asked.
    assert_eq!(
        of("/u/aterm/.claude/worktrees/v/tmp/x").as_deref(),
        Some(Path::new("/u/aterm/.claude/worktrees/v"))
    );
    assert!(asked_of().is_empty());
    // A tab whose directory is there, in ~/aterm or in ~/aterm itself, still holds its builds.
    assert_eq!(
        of("/u/aterm/crates/x").as_deref(),
        Some(Path::new("/u/aterm"))
    );
    assert_eq!(asked_of(), [PathBuf::from("/u/aterm/crates/x")]);
    assert_eq!(of("/u/aterm").as_deref(), Some(Path::new("/u/aterm")));
    assert!(asked_of().is_empty());
}

#[test]
fn a_checkouts_root_is_read_off_its_git_directory_or_a_worktrees_git_file() {
    let dir = aterm_tempfile::TempDir::new().expect("owned checkouts");
    let repo = dir.path().join("repo");
    let wt = repo.join(".claude").join("worktrees").join("w");
    std::fs::create_dir_all(repo.join(".git")).expect("the checkout's .git");
    std::fs::create_dir_all(wt.join("crates")).expect("the worktree");
    std::fs::write(wt.join(".git"), "gitdir: ../../../.git/worktrees/w\n")
        .expect("the worktree's .git file");
    assert!(checkout_root(&repo));
    assert!(checkout_root(&wt));
    assert!(!checkout_root(&wt.join("crates")));
    let holders = [Hog {
        built_s_ago: None,
        path: repo.join("target"),
        bytes: 61 << 30,
        bytes_partial: false,
        why: String::new(),
        serves: Some(repo.clone()),
    }];
    let of = |cwd: &Path| checkout_of(&holders, cwd, None, &checkout_root, &dir_there);
    let checkout = of(&wt.join("crates"));
    assert_eq!(checkout.as_deref(), Some(wt.as_path()));
    assert_eq!(held_in(&holders, checkout.as_deref()), 0);
    std::fs::create_dir_all(repo.join("crates")).expect("the checkout's own directory");
    let checkout = of(&repo.join("crates"));
    assert_eq!(held_in(&holders, checkout.as_deref()), 61 << 30);
    // The worktree removed under its session: no `.git` is left to find, and its directory is
    // gone, so it is not read as the checkout around it.
    assert!(dir_there(&wt.join("crates")));
    std::fs::remove_dir_all(&wt).expect("the worktree removed");
    assert!(!dir_there(&wt.join("crates")));
    assert_eq!(of(&wt.join("crates")), None);
    assert_eq!(of(&wt), None);
    assert_eq!(of(&repo).as_deref(), Some(repo.as_path()));
}

/// A lease in `state` that owes host `host`'s session `sid` its line on volume `key`, elected
/// at `now`.
fn owed_at(state: &Path, key: &str, host: &ask::HostId, sid: &str, now: i64) {
    let pick = ask::Pick {
        host: host.clone(),
        sid: sid.to_owned(),
    };
    let lease = ask::Lease {
        schema: 0,
        volumes: vec![ask::Entry {
            key: key.to_owned(),
            episode_ms: Some(now - ask::NOMINATE_MS),
            done: Some(ask::Done::Spent),
            phase: ask::Phase::Owed {
                pick: pick.clone(),
                elected_ms: now,
                until_ms: now + ask::DELIVER_MS,
                orphan_ms: None,
            },
            picks: vec![pick],
            ..ask::Entry::default()
        }],
    };
    ask::transact(state, &ask::LOCK_LADDER, |l| *l = lease)
        .expect("the lease is written")
        .expect("nobody holds its lock");
}

#[test]
fn lease_read_failure_does_not_count_as_an_idle_lease() {
    let state = aterm_tempfile::TempDir::new().expect("lease directory");
    let path = state.path().join(ask::LEASE_FILE);
    let key = "dev:failed-lease-read";
    assert!(look_rests(state.path(), key, Level::Ok, false));
    std::fs::write(&path, ask::Lease::default().encode()).expect("valid empty lease");
    assert!(look_rests(state.path(), key, Level::Ok, false));
    for text in ["not json", r#"{"schema":2,"volumes":[]}"#] {
        std::fs::write(&path, text).expect("bad lease");
        assert!(!look_rests(state.path(), key, Level::Ok, false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
    std::fs::remove_file(&path).expect("remove owned fixture");
    std::fs::create_dir(&path).expect("non-file lease");
    assert!(!look_rests(state.path(), key, Level::Ok, false));
    assert!(path.is_dir());
}

const GIB: u64 = 1 << 30;

#[test]
fn a_typed_line_records_the_free_figure_read_as_it_went_not_the_boards() {
    let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
    let host = ask::HostId {
        pid: 7,
        start: "start-7".to_owned(),
    };
    let now = now_ms();
    for (key, measured, recorded) in [
        ("dev:free-measured", Some(40 * GIB), 40 * GIB),
        ("dev:free-unmeasured", None, 10 * GIB),
    ] {
        owed_at(state.path(), key, &host, "s-fresh", now);
        let owed = crate::disk_board::AskOwed {
            key: key.to_owned(),
            text: "the line".to_owned(),
        };
        let at = AtBreak {
            state: state.path(),
            host: &host,
            sid: "s-fresh",
            owed: &owed,
            now,
            unfit: None,
            held: None,
            free: 10 * GIB,
            level: Level::Short,
        };
        assert_eq!(at.step(|| "typed".to_owned(), || measured), "typed");
        let lease = ask::read(state.path()).expect("valid fixture lease");
        assert!(
            matches!(
                &lease.entry(key).expect("entry").phase,
                ask::Phase::Asked { free_at, best_free, .. }
                    if *free_at == recorded && *best_free == recorded
            ),
            "{key}: the rise is measured from the line's own reading, the board's a fallback: \
             {lease:?}"
        );
        assert_eq!(typed_last(key).map(|t| t.free), Some(recorded), "{key}");
    }
}

/// A line the server's observe gate withholds (the session's agent is unconfirmed,
/// [`aterm_agent::harness::agent_image::WITHHELD`]) is handed on by the lease, as a session
/// that may not be asked is, and the step says why, `released:unconfirmed`: nothing is
/// typed, and the session holds it no more. NEGATIVE CONTROL (run): with the withheld step
/// returned as the step's word, the line stays claimed by the observed session.
#[test]
fn a_line_the_observe_gate_withholds_is_handed_on() {
    let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
    let host = ask::HostId {
        pid: 9,
        start: "start-9".to_owned(),
    };
    let now = now_ms();
    let key = "dev:observed-break";
    owed_at(state.path(), key, &host, "s-observed", now);
    let owed = crate::disk_board::AskOwed {
        key: key.to_owned(),
        text: "the line".to_owned(),
    };
    let at = AtBreak {
        state: state.path(),
        host: &host,
        sid: "s-observed",
        owed: &owed,
        now,
        unfit: None,
        held: None,
        free: 10 * GIB,
        level: Level::Short,
    };
    assert_eq!(
        at.step(
            || aterm_agent::harness::agent_image::WITHHELD.to_owned(),
            || panic!("measured a line that never went")
        ),
        RELEASED_UNCONFIRMED
    );
    let lease = ask::read(state.path()).expect("valid fixture lease");
    let phase = lease.entry(key).map(|e| &e.phase);
    assert!(
        !matches!(phase, Some(ask::Phase::Owed { pick, .. } | ask::Phase::Asked { pick, .. })
            if pick.sid == "s-observed"),
        "handed on: {phase:?}"
    );
    assert!(typed_last(key).is_none(), "nothing typed");
}

#[test]
fn a_turn_break_the_loop_holds_types_nothing_and_claims_nothing() {
    let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
    let host = ask::HostId {
        pid: 8,
        start: "start-8".to_owned(),
    };
    let now = now_ms();
    let key = "dev:held-break";
    owed_at(state.path(), key, &host, "s-held", now);
    let before = ask::read(state.path()).expect("valid fixture lease");
    let owed = crate::disk_board::AskOwed {
        key: key.to_owned(),
        text: "the line".to_owned(),
    };
    let at = |unfit: Option<&'static str>, held: Option<&'static str>| AtBreak {
        state: state.path(),
        host: &host,
        sid: "s-held",
        owed: &owed,
        now,
        unfit,
        held,
        free: 10 * GIB,
        level: Level::Short,
    };
    for why in ["person", "exchange", "managed"] {
        assert_eq!(
            at(None, Some(why)).step(
                || panic!("typed at a turn break {why} holds"),
                || panic!("measured at a turn break {why} holds")
            ),
            format!("wait:{why}")
        );
    }
    assert_eq!(
        ask::read(state.path()).expect("valid fixture lease"),
        before,
        "unclaimed, so its time is not stretched, and still owed"
    );
    assert_eq!(
        at(Some("task-done"), Some("person")).step(|| panic!("typed a barred session"), || None),
        "released:task-done",
        "a session the seat rule bars is released whatever holds its turn break"
    );
    assert!(
        !matches!(
            ask::read(state.path())
                .expect("valid fixture lease")
                .entry(key)
                .map(|e| &e.phase),
            Some(ask::Phase::Owed { .. })
        ),
        "released"
    );
}

/// What a session's worker found of the session itself at its last turn break (its task
/// finished, nobody gave it one) holds only until it works again, as a session that works was
/// given something to do: a look while its agent reads busy seats it. Whose it is (the
/// person's own exchange, a manager's wait) holds across its turns until its next turn break.
/// A finding kept mid-turn barred a tab through the whole of the turn its first task started,
/// so an episode that opened then found nobody to ask, said only the owner could free the
/// space, and stayed out for its cooldown.
#[test]
fn a_finding_of_the_session_itself_holds_until_it_works_and_whose_it_is_across_turns() {
    use crate::disk_board::Unfit;
    let own = |why| Unfit {
        own: Some(why),
        theirs: None,
    };
    for why in ["taskless", "finished", "autopilot-off"] {
        assert_eq!(
            own(why).bars(false),
            Some(why),
            "resting, or held at a wall"
        );
        assert_eq!(own(why).bars(true), None, "{why} while it works: seated");
    }
    let theirs = Unfit {
        own: None,
        theirs: Some("exchange"),
    };
    assert_eq!(theirs.bars(false), Some("exchange"));
    assert_eq!(theirs.bars(true), Some("exchange"), "theirs while it works");
    let both = Unfit {
        own: Some("taskless"),
        theirs: Some("managed"),
    };
    assert_eq!(both.bars(false), Some("taskless"));
    assert_eq!(both.bars(true), Some("managed"));
    assert_eq!(Unfit::default().bars(false), None);
    // Through the look: the same tab found taskless, at rest and then working.
    let mut sessions = crate::session_store::SessionStore::default();
    let h = crate::session_store::test_handle(9_100);
    sessions.register(h.clone());
    let sid = h.sid.as_str().to_owned();
    let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
    let key = "fsid:9100";
    let reads = |word: &'static str, seq: u64| {
        h.ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .publish_agent(
                word,
                None,
                None,
                Some(Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq },
                    fp: seq,
                },
            );
        bar_at(&store, &sid, key)
    };
    crate::disk_board::SHELF
        .board_lock()
        .unfit
        .insert(sid.clone(), own("taskless"));
    let resting = reads("idle", 1);
    let working = reads("busy", 2);
    crate::disk_board::SHELF.board_lock().unfit.remove(&sid);
    assert_eq!(working, None, "given a task since, it may be asked");
    assert_eq!(resting, Some("taskless"), "resting where it was found");
}

/// A look that reads the volume clear takes no lock only while the lease holds the last line
/// this process typed on it ([`look_rests`]). A line whose record a full disk lost, and whose
/// episode another host ended as the space came back, steps the lease at this process's next
/// look, which records it, so its cooldown holds the next shortage shut. The look before
/// rested on such a lease, as no episode was open, and the next shortage asked a second agent
/// inside the cooldown.
#[test]
fn a_clear_look_steps_a_lease_that_lacks_this_processs_last_line_and_rests_once_it_holds_it() {
    let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
    let key = "dev:rests-unrecorded";
    let host = ask::HostId {
        pid: 1,
        start: String::new(),
    };
    let at = 1_000_000;
    assert!(
        look_rests(state.path(), key, Level::Ok, false),
        "nothing to step"
    );
    assert!(!look_rests(state.path(), key, Level::Short, false), "short");
    TYPED.lock().unwrap_or_else(PoisonError::into_inner).insert(
        key.to_owned(),
        Typed {
            sid: "s-a".to_owned(),
            at_ms: at,
            free: 0,
        },
    );
    // An earlier line of the volume's, its episode ended: no episode open, the lease still
    // without the line this process typed since.
    ask::transact(state.path(), &ask::LOCK_LADDER, |l| {
        ask::typed(l, key, &host, "s-b", at - 60_000, 0);
    })
    .expect("written")
    .expect("locked");
    let lacks = look_rests(state.path(), key, Level::Ok, false);
    ask::transact(state.path(), &ask::LOCK_LADDER, |l| {
        ask::typed_unrecorded(l, key, &host, "s-a", at, 0)
    })
    .expect("written")
    .expect("locked");
    let holds = look_rests(state.path(), key, Level::Ok, false);
    TYPED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(key);
    assert!(!lacks, "the lease lacks the line: the look steps it");
    assert!(holds, "recorded: the look rests");
    assert_eq!(
        ask::read(state.path())
            .expect("valid fixture lease")
            .entry(key)
            .and_then(|e| e.last_ask_ms),
        Some(at),
        "its cooldown runs from the lost line"
    );
}

#[test]
fn a_filling_note_is_said_when_it_changes_and_its_end_once() {
    let last = Mutex::new(None);
    let told = std::sync::Arc::new(Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&told);
    let note = move |n: HostNote| sink.lock().unwrap_or_else(PoisonError::into_inner).push(n);
    let filling = |eta_s: i64, needed: bool| HostNote::DiskFilling {
        needed,
        projected_only: !needed,
        free: 75 << 30,
        floor: 47 << 30,
        rate_bps: Some(39 << 20),
        eta_s: Some(eta_s),
        said: None,
    };
    say_filling(&last, &note, None);
    assert!(
        told.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no end before a start"
    );
    say_filling(&last, &note, Some(filling(780, false)));
    say_filling(&last, &note, Some(filling(780, false)));
    say_filling(&last, &note, Some(filling(720, false)));
    say_filling(&last, &note, Some(filling(720, true)));
    say_filling(&last, &note, None);
    say_filling(&last, &note, None);
    let told = told.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(told.len(), 4, "{told:#?}");
    assert!(matches!(
        told[2],
        HostNote::DiskFilling { needed: true, .. }
    ));
    assert_eq!(told[3], HostNote::DiskFillingOver);
}

#[test]
fn a_projected_look_never_rests() {
    let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
    let key = "dev:rests-projected";
    assert!(look_rests(state.path(), key, Level::Low, false));
    assert!(
        !look_rests(state.path(), key, Level::Low, true),
        "a crossing projected steps the lease, so an episode opens before the floor"
    );
}

#[test]
fn a_spawn_failure_is_said_as_one_plain_word() {
    assert_eq!(
        spawn_word("no window to host the session"),
        "no-window-to-host-the-session"
    );
    assert_eq!(
        spawn_word("`claude` not found: $PATH"),
        "claude-not-found-path"
    );
    assert_eq!(spawn_word("\u{1b}[31m"), "31m");
    assert_eq!(spawn_word("!!!"), "unknown");
    assert!(spawn_word(&"x ".repeat(100)).len() <= 48);
}

#[test]
fn the_filling_line_says_the_ask_then_the_worker() {
    let host = ask::HostId {
        pid: 7,
        start: "s".to_owned(),
    };
    let mut entry = ask::Entry::default();
    assert_eq!(filling_said(None, Some(&entry), &[]), None);
    assert_eq!(
        filling_said(Some("asked @a"), None, &[]).as_deref(),
        Some("asked @a")
    );
    entry.worker = Some(ask::Worker {
        host,
        at_ms: 0,
        state: ask::WorkerState::Running {
            sid: "s-w".to_owned(),
            started_ms: 0,
            free_at: 0,
            best_free: 0,
        },
        orphan_ms: None,
        free_at_claim: None,
        target: None,
    });
    assert_eq!(
        filling_said(Some("asked @a"), Some(&entry), &[]).as_deref(),
        Some("asked @a; aterm's disk worker @s-w is freeing space")
    );
    let report = report_lines(
        "Nothing left: freed ~/Library/Caches/pip 3.1 GiB\n\
         - disk-free filling: a Docker VM image in ~/Library/Containers grows 30 GiB/h\n\
         disk-free owner-only: the VM image \u{1b}[31m`rm -rf ~`\n",
    );
    assert_eq!(
        report,
        [
            "disk-free filling: a Docker VM image in ~/Library/Containers grows 30 GiB/h",
            "disk-free owner-only: the VM image 31m rm -rf ~",
        ]
    );
    assert!(
        filling_said(None, None, &report)
            .is_some_and(|s| s.starts_with("its worker said disk-free filling:"))
    );
    assert!(report_lines("no report here").is_empty());
}
