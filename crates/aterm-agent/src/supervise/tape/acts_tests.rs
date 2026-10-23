// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Counterfactual cursor tests over hand-made tapes.

use super::*;
use crate::supervise::CtlReply;
use crate::supervise::tape::codec::{ctl_reply, ctl_request};

/// A control round trip asking `words`, its reply `OK <what>`.
fn ctl(words: &[&str], what: &str) -> EffectRecord {
    EffectRecord::Exchange {
        door: doors::CTL.to_string(),
        request: ctl_request(words),
        reply: ctl_reply(&Ok(CtlReply {
            code: 0,
            stdout: format!("OK {what}\n"),
            stderr: String::new(),
        })),
    }
}

/// The reply `ctl(_, what)` holds.
fn said(what: &str) -> Vec<u8> {
    ctl_reply(&Ok(CtlReply {
        code: 0,
        stdout: format!("OK {what}\n"),
        stderr: String::new(),
    }))
}

/// A cursor over `records`, none stamped.
fn cursor(records: Vec<EffectRecord>) -> ActsCursor {
    let n = records.len();
    ActsCursor::new(records, vec![None; n])
}

/// Ask the control lane `words`.
fn ask(c: &mut ActsCursor, words: &[&str]) -> Result<Served, Refusal> {
    c.exchange(doors::CTL, &ctl_request(words))
}

/// Each request's class, as §5.1 has it: writes, signals, restarts and the
/// host's acts are acts; the claim's renewal and the teller are housekeeping;
/// `await` waits; a line printed and the bound are outputs; the rest reads.
#[test]
fn requests_are_classed_as_the_plan_classes_them() {
    let class = |words: &[&str]| act_class(doors::CTL, &ctl_request(words));
    for act in [
        &["@s-1", "key", "1"][..],
        &["send", "hi"],
        &["post", "@s-2", "x"],
        &["meta", "set", "attention", "owner=supervisor"],
        &["signal", "INT"],
        &["restart", "--pid", "9"],
    ] {
        assert_eq!(class(act), ActClass::Act, "{act:?}");
    }
    for keep in [
        &["meta", "set", "supervisor", "aterm-supervise:1"][..],
        &["@s-1", "meta", "unset", "supervisor"],
        &["story", "approved"],
    ] {
        assert_eq!(class(keep), ActClass::Housekeeping, "{keep:?}");
    }
    assert_eq!(
        class(&["await", "seq", "4", "timeout", "20000"]),
        ActClass::Wait
    );
    for read in [&["text", "--json"][..], &["meta"], &["status"], &["who"]] {
        assert_eq!(class(read), ActClass::Read, "{read:?}");
    }
    assert_eq!(
        act_class(doors::TELLER, b"anything"),
        ActClass::Housekeeping
    );
    assert_eq!(act_class(doors::OUT, b"a line"), ActClass::Output);
    assert_eq!(
        act_class(doors::WRITE_BOUND, b"@ledger/x"),
        ActClass::Output
    );
    assert_eq!(act_class(doors::PROBE_HIDDEN_TWIN, b"/x"), ActClass::Read);
    let host_act = crate::supervise::run::host_door::HOST_ACTS[0];
    assert_eq!(act_class(host_act, b""), ActClass::Act);
    assert_eq!(act_class("host.wants", b""), ActClass::Read);
}

/// A run that asks what the recorded one asked, in its order, takes every
/// record as a strict replay would: the account says EXACT.
#[test]
fn an_unchanged_run_is_served_record_for_record() {
    let mut c = ActsCursor::new(
        vec![
            EffectRecord::Mono(5),
            ctl(&["text"], "box"),
            EffectRecord::Clock(7_000_000),
            EffectRecord::Emit {
                door: doors::JOURNAL.to_string(),
                bytes: b"row".to_vec(),
            },
            ctl(&["key", "1"], "pressed"),
            ctl(&["text"], "busy"),
        ],
        vec![None; 6],
    );
    assert_eq!(c.mono(), 5);
    assert_eq!(ask(&mut c, &["text"]).map(|s| s.reply), Ok(said("box")));
    assert_eq!(c.clock(), 7_000_000);
    assert_eq!(c.emit(doors::JOURNAL), Some(3));
    assert_eq!(ask(&mut c, &["key", "1"]).map(|s| s.at), Ok(Some(4)));
    assert_eq!(ask(&mut c, &["text"]).map(|s| s.reply), Ok(said("busy")));
    assert_eq!(c.position(), 6);
    assert!(c.account.exact(), "{:?}", c.account);
    assert_eq!((c.account.taken, c.account.acts), (1, 1));
}

/// Within its segment a read is resynced forward (what it skips dropped) or
/// served again; it never crosses the act, and a read the segment does not
/// hold is STALE — a read held only past the act included (a policy that
/// looks once more before it acts asks one; it skipped nothing yet).
#[test]
fn a_read_is_served_from_its_own_segment_alone() {
    let tape = || {
        cursor(vec![
            ctl(&["text"], "one"),
            ctl(&["meta"], "m"),
            ctl(&["key", "1"], "pressed"),
            ctl(&["status"], "after"),
        ])
    };
    let mut c = tape();
    assert_eq!(ask(&mut c, &["meta"]).map(|s| s.reply), Ok(said("m")));
    assert_eq!(c.account.dropped, 1, "the text before it dropped");
    assert_eq!(ask(&mut c, &["meta"]).map(|s| s.reply), Ok(said("m")));
    assert_eq!(c.account.repeated, 1, "served again");
    assert_eq!(
        ask(&mut c, &["text"]).map(|s| s.reply),
        Ok(said("one")),
        "a dropped read is before the cursor: served again, never from the future"
    );
    let mut c = tape();
    assert_eq!(
        ask(&mut c, &["status"]).err(),
        Some(Refusal::Stale {
            class: ActClass::Read
        }),
        "held only past the act: STALE"
    );
    assert_eq!(c.position(), 0, "and nothing consumed");
}

/// A WAIT the recorded run waited for only after the segment's act — the
/// very request, or the same wait with another timeout, past the act — is
/// that act SKIPPED: the candidate went on without it. A wait held nowhere,
/// and a file read held only past the act, are STALE.
#[test]
fn a_wait_held_only_past_the_act_is_the_act_skipped() {
    let wait = ["await", "seq", "102", "timeout", "20000"];
    let mut c = cursor(vec![
        ctl(&["text"], "worked"),
        ctl(&["turn", "keep going"], "typed"),
        ctl(&wait, "woke"),
        EffectRecord::File {
            path: "/s/notes".to_string(),
            bytes: b"n".to_vec(),
        },
    ]);
    assert!(ask(&mut c, &["text"]).is_ok());
    assert_eq!(
        ask(&mut c, &wait).err(),
        Some(Refusal::Skipped {
            closing: 1,
            class: ActClass::Wait
        })
    );
    assert_eq!(
        ask(&mut c, &["@s-1", "await", "seq", "102", "timeout", "5000"]).err(),
        Some(Refusal::Skipped {
            closing: 1,
            class: ActClass::Wait
        }),
        "the same wait, its timeout aside"
    );
    assert_eq!(
        ask(&mut c, &["await", "seq", "103", "timeout", "20000"]).err(),
        Some(Refusal::Stale {
            class: ActClass::Wait
        }),
        "another screen's wait, held nowhere: STALE"
    );
    assert_eq!(
        c.file("/s/notes").err(),
        Some(Refusal::Stale {
            class: ActClass::Read
        })
    );
    assert_eq!(c.position(), 1, "nothing past the act consumed");
}

/// Whether the tape is SPENT where the cursor stands: no round trip left to
/// ask and no file to read — a clock or an output after the cursor does not
/// count, an exchange does.
#[test]
fn the_tape_is_spent_when_only_clocks_and_outputs_are_left() {
    let mut c = cursor(vec![
        ctl(&["text"], "t"),
        EffectRecord::Mono(5),
        EffectRecord::Emit {
            door: doors::JOURNAL.to_string(),
            bytes: b"row".to_vec(),
        },
    ]);
    assert!(!c.spent(), "a read is left");
    assert!(ask(&mut c, &["text"]).is_ok());
    assert!(c.spent(), "a clock and an output are left");
    let c = cursor(vec![ctl(&["key", "1"], "pressed")]);
    assert!(!c.spent(), "an act is left");
}

/// An act must be the segment's closing act: taken with its reads unasked
/// (dropped), or refused naming that act; one past the tape's last act names
/// none.
#[test]
fn an_act_is_the_segments_closing_act_or_the_first_divergence() {
    let tape = || {
        cursor(vec![
            ctl(&["text"], "box"),
            ctl(&["key", "1"], "pressed"),
            ctl(&["text"], "busy"),
        ])
    };
    let mut c = tape();
    assert_eq!(ask(&mut c, &["key", "1"]).map(|s| s.at), Ok(Some(1)));
    assert_eq!((c.account.dropped, c.segment_start()), (1, 2));
    assert_eq!(
        ask(&mut c, &["key", "1"]).err(),
        Some(Refusal::ActDiffers { closing: None }),
        "past the last act"
    );
    let mut c = tape();
    assert_eq!(
        ask(&mut c, &["key", "2"]).err(),
        Some(Refusal::ActDiffers { closing: Some(1) })
    );
    assert_eq!(
        ask(&mut c, &["meta", "set", "attention", "owner=supervisor"]).err(),
        Some(Refusal::ActDiffers { closing: Some(1) }),
        "another kind of act"
    );
    assert_eq!(c.acts_left(), (Some(1), 1));
}

/// A derived probe the segment does not hold is answered from before the
/// cursor by astream's keyed view's rule — the latest recorded answer, never
/// one from past the cursor.
#[test]
fn a_probe_is_answered_from_before_the_cursor() {
    let probe = |reply: &[u8]| EffectRecord::Exchange {
        door: doors::PROBE_HIDDEN_TWIN.to_string(),
        request: b"/w/x".to_vec(),
        reply: reply.to_vec(),
    };
    let mut c = cursor(vec![
        probe(b"first"),
        ctl(&["key", "1"], "pressed"),
        ctl(&["text"], "t"),
        ctl(&["key", "2"], "pressed"),
        probe(b"later"),
    ]);
    assert!(ask(&mut c, &["key", "1"]).is_ok());
    let served = c
        .exchange(doors::PROBE_HIDDEN_TWIN, b"/w/x")
        .expect("served");
    assert_eq!(served.reply, b"first".to_vec());
    assert_eq!((c.account.keyed, c.position()), (1, 2));
    assert!(
        c.exchange(doors::PROBE_HIDDEN_TWIN, b"/w/other").is_err(),
        "a look never recorded is STALE"
    );
}

/// A wait with other arguments takes the segment's next wait of its kind
/// (schedule taint); a wait served again moves the virtual clock by its
/// timeout.
#[test]
fn a_wait_with_other_arguments_is_tainted() {
    let wait = |seq: &str| ctl(&["await", "seq", seq, "timeout", "20000"], "woke");
    let mut c = ActsCursor::new(
        vec![wait("3"), ctl(&["text"], "t"), wait("4")],
        vec![
            Some(Stamp {
                mono_ns: 1_000,
                wall_ms: 10,
            }),
            None,
            None,
        ],
    );
    assert!(
        c.exchange(
            doors::CTL,
            &ctl_request(&["await", "seq", "9", "timeout", "5"])
        )
        .is_ok()
    );
    assert_eq!((c.account.tainted, c.position()), (1, 1));
    assert_eq!(c.now().0, 1_000, "the stamp of the record taken");
    assert!(
        c.exchange(
            doors::CTL,
            &ctl_request(&["await", "seq", "3", "timeout", "20000"])
        )
        .is_ok()
    );
    assert_eq!(c.account.repeated, 1);
    assert_eq!(c.now().0, 1_000 + 20_000 * 1_000_000);
    assert!(
        ask(&mut c, &["await", "gone", "x", "timeout", "1"]).is_err(),
        "no wait of its kind: STALE"
    );
}

/// A clock read with no clock record under the cursor is virtual: the last
/// record's stamp plus the sleeps since, never running back; a sleep with no
/// identical one under the cursor adds to it.
#[test]
fn an_extra_clock_read_is_virtual_time() {
    let mut c = ActsCursor::new(
        vec![ctl(&["text"], "t"), EffectRecord::Mono(9_000)],
        vec![
            Some(Stamp {
                mono_ns: 4_000,
                wall_ms: 2,
            }),
            None,
        ],
    );
    assert!(ask(&mut c, &["text"]).is_ok());
    assert_eq!(c.clock(), 2_000_000, "the stamp's wall time");
    c.exchange(doors::SLEEP, b"500")
        .expect("a sleep is always served");
    assert_eq!(c.now(), (4_500, 2_000_500));
    assert_eq!(c.mono(), 9_000, "the record under the cursor");
    assert_eq!(c.mono(), 9_000, "never back to the virtual 4 500");
    assert_eq!((c.account.virtual_clock, c.account.slept), (2, 1));
}

/// A flag is raised at the read the recorded run saw it at (its false reads
/// counted), and once the cursor has passed its record; while the loop
/// follows the tape, the virtual clock does not raise it early.
#[test]
fn a_raised_flag_follows_the_cursor() {
    let stop = |false_reads| EffectRecord::Raised {
        door: doors::RAISED_STOP.to_string(),
        false_reads,
    };
    let mut c = ActsCursor::new(
        vec![ctl(&["text"], "t"), stop(1), ctl(&["status"], "s")],
        vec![
            Some(Stamp {
                mono_ns: 50,
                wall_ms: 0,
            }),
            Some(Stamp {
                mono_ns: 50,
                wall_ms: 0,
            }),
            None,
        ],
    );
    assert!(!c.raised(doors::RAISED_STOP), "before its record");
    assert!(ask(&mut c, &["text"]).is_ok());
    assert!(!c.raised(doors::RAISED_STOP), "the counted false read");
    assert!(c.raised(doors::RAISED_STOP), "the read that saw it");
    assert!(c.account.exact(), "{:?}", c.account);
    let mut c = ActsCursor::new(
        vec![ctl(&["text"], "t"), stop(0), ctl(&["status"], "s")],
        vec![None; 3],
    );
    assert!(ask(&mut c, &["status"]).is_ok());
    assert!(c.raised(doors::RAISED_STOP), "passed by a resync");
}

/// Housekeeping is served from any segment before the loop's own; an output
/// is never compared, its outcome the last one recorded at its door or
/// success.
#[test]
fn housekeeping_and_outputs_are_never_compared() {
    let renew = ["meta", "set", "supervisor", "h"];
    let mut c = cursor(vec![
        ctl(&renew, "renewed"),
        ctl(&["key", "1"], "pressed"),
        EffectRecord::Exchange {
            door: doors::OUT.to_string(),
            request: b"APPROVED".to_vec(),
            reply: Vec::new(),
        },
    ]);
    assert!(ask(&mut c, &renew).is_ok());
    assert!(ask(&mut c, &["key", "1"]).is_ok());
    assert_eq!(
        ask(&mut c, &renew).map(|s| s.reply),
        Ok(said("renewed")),
        "from the segment before"
    );
    assert_eq!(c.account.housekept, 1);
    let line = c.exchange(doors::OUT, b"another line").expect("printed");
    assert_eq!(
        (line.at, c.position()),
        (Some(2), 3),
        "under the cursor: taken"
    );
    let more = c.exchange(doors::NOTES, b"a note").expect("printed");
    assert_eq!((more.reply, more.at), (Vec::new(), None), "success");
    assert_eq!(c.emit(doors::LEDGER), None);
    assert_eq!(c.account.unmatched_outputs, 2);
}

/// HOUSEKEEPING IS NEVER REFUSED (the plan's §5.1: never compared): the
/// claim's release where the recorded run had not released it — its release
/// past the act, or nowhere — is answered with the latest of its kind before
/// the cursor, else its door's success, and so is a teller's story in other
/// words; nothing is served from past the act, and the cursor stays.
#[test]
fn housekeeping_is_never_refused() {
    let renew = ["meta", "set", "supervisor", "h"];
    let mut c = cursor(vec![
        ctl(&renew, "renewed"),
        ctl(&["key", "1"], "pressed"),
        ctl(&["meta", "unset", "supervisor", "holder=h"], "released"),
    ]);
    assert!(ask(&mut c, &renew).is_ok());
    let released = ask(&mut c, &["meta", "unset", "supervisor", "holder=h"])
        .expect("housekeeping is never refused");
    assert_eq!(
        (
            released.at,
            super::codec::ctl_reply_of(&released.reply)
                .ok()
                .and_then(Result::ok)
                .map(|r| r.code)
        ),
        (None, Some(0)),
        "its door's success, never the release past the act"
    );
    let renewed = ask(&mut c, &["meta", "set", "supervisor", "h", "ttl=1"])
        .expect("a renewal in other words");
    assert_eq!(
        (renewed.at, renewed.reply),
        (Some(0), said("renewed")),
        "the latest of its kind before the cursor"
    );
    let told = c
        .exchange(doors::TELLER, &ctl_request(&["story", "another", "word"]))
        .expect("a story in other words");
    assert_eq!(told.at, None);
    assert_eq!((c.position(), c.account.housekept), (1, 3));
    assert_eq!(c.acts_left(), (Some(1), 1), "the act still to take");
}

/// THE PROGRESS GUARD: a counterfactual served on and on with nothing
/// consumed — one read asked again for ever — ends DIVERGED (no progress)
/// past the guard's count, and never runs for ever (the one-minute wait is
/// the hang detector).
#[test]
fn a_candidate_that_asks_again_for_ever_ends_at_the_progress_guard() {
    use crate::supervise::fx::Fx;
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::{PROGRESS_READS, ReplayCore};
    use crate::supervise::tape::report::Cause;
    let core = ReplayCore::start_acts(
        vec![ctl(&["text"], "t"), ctl(&["key", "1"], "pressed")],
        Vec::new(),
        VERBATIM,
        VERBATIM,
        "test",
        None,
    );
    let fx = Fx::replay(std::sync::Arc::clone(&core));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loop {
                let _: Result<CtlReply, String> = fx.exchange(
                    doors::CTL,
                    || ctl_request(&["text"]),
                    || unreachable!("a replay makes no live call"),
                );
            }
        }));
        let _ = tx.send(ended.is_err());
    });
    let unwound = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the guard ended the replay");
    assert!(unwound, "the replay unwound from the guard");
    let divergence = core.diverged().expect("latched");
    assert!(
        matches!(divergence.cause, Cause::NoProgress { reads } if reads > PROGRESS_READS),
        "{divergence:?}"
    );
}

/// A DERIVED PROBE'S ANSWER FROM BEFORE THE CURSOR is astream-effects' own:
/// at every position of generated tapes — probes at two doors over a few
/// requests, control reads and acts between them — the cursor's keyed answer
/// (read from its own index) is the record and reply `KeyedView::lookup`
/// gives with `seen` the cursor's position, a miss for a miss. The negative
/// control: an answer that also admits the record under the cursor (an
/// inclusive bound, one record from the future) disagrees with astream on
/// the same tapes.
#[test]
fn a_keyed_answer_is_astreams_at_every_cursor() {
    use astream_effects::KeyedView;
    use aterm_spec::rng::XorShift;
    let probe_doors = [doors::PROBE_HIDDEN_TWIN, doors::PROBE_ZONE_OFFSET];
    let requests: [&[u8]; 3] = [b"/w/a", b"/w/b", b"+0000"];
    let mut rng = XorShift::new(83);
    let mut inclusive_disagreed = false;
    for case in 0..64 {
        let len = rng.range(1, 48);
        let records: Vec<EffectRecord> = (0..len)
            .map(|i| match rng.below(4) {
                0 => ctl(&["key", "1"], "pressed"),
                1 => ctl(&["text"], &format!("{i}")),
                _ => EffectRecord::Exchange {
                    door: (*rng.pick(&probe_doors)).to_string(),
                    request: rng.pick(&requests).to_vec(),
                    reply: format!("{case}:{i}").into_bytes(),
                },
            })
            .collect();
        let view = KeyedView::new(&records);
        let mut c = cursor(records.clone());
        for pos in 0..=records.len() {
            c.pos = pos;
            for door in probe_doors {
                for request in requests {
                    let theirs = view.lookup(door, request, pos).ok();
                    let ours = c.keyed(door, request);
                    assert_eq!(
                        ours,
                        theirs.map(|h| h.position),
                        "case {case}, pos {pos}, {door} {request:?}"
                    );
                    if let (Some(at), Some(hit)) = (ours, theirs) {
                        assert_eq!(c.reply(at), hit.reply, "case {case}, pos {pos}");
                    }
                    let inclusive = last_in(c.asked(door, request), 0, pos + 1);
                    inclusive_disagreed |= inclusive != theirs.map(|h| h.position);
                }
            }
        }
    }
    assert!(
        inclusive_disagreed,
        "the negative control: an inclusive bound answers from the future somewhere"
    );
}
